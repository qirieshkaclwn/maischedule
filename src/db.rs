use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::models::{ChangeRecord, GroupInfo, GroupSchedule, User};


#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<rusqlite::Connection>>,
}

impl Database {
    pub fn new(db_path: &str) -> Result<Self> {
        if db_path != ":memory:" {
            if let Some(parent) = Path::new(db_path).parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
        }

        let conn = rusqlite::Connection::open(db_path)
            .with_context(|| format!("Не удалось открыть SQLite базу данных: {}", db_path))?;

        // Оптимизация памяти SQLite
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA cache_size = -1000;
            PRAGMA temp_store = MEMORY;
            "#,
        )?;

        // Создание таблиц и индексов
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schedule_snapshots (
                group_name TEXT PRIMARY KEY,
                schedule_json TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS mai_groups (
                name TEXT PRIMARY KEY,
                name_lower TEXT NOT NULL DEFAULT '',
                fac TEXT NOT NULL DEFAULT '',
                level TEXT NOT NULL DEFAULT '',
                course TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS users (
                id INTEGER PRIMARY KEY,
                group_name TEXT,
                username TEXT,
                first_name TEXT,
                notifications_enabled INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS subscribers (
                chat_id INTEGER PRIMARY KEY,
                group_name TEXT NOT NULL,
                username TEXT,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS change_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                group_name TEXT NOT NULL,
                change_type TEXT NOT NULL,
                description TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS app_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_users_group ON users(group_name);
            CREATE INDEX IF NOT EXISTS idx_mai_groups_lower ON mai_groups(name_lower);
            CREATE INDEX IF NOT EXISTS idx_mai_groups_course ON mai_groups(course);
            CREATE INDEX IF NOT EXISTS idx_mai_groups_fac ON mai_groups(fac);
            CREATE INDEX IF NOT EXISTS idx_subscribers_group ON subscribers(group_name);
            CREATE INDEX IF NOT EXISTS idx_change_history_group_id ON change_history(group_name, id DESC);
            "#,
        )?;

        // Автоматическая миграция подписчиков из legacy таблицы subscribers в users
        let _ = conn.execute(
            r#"
            INSERT OR IGNORE INTO users (id, group_name, username, first_name, notifications_enabled, created_at, updated_at)
            SELECT chat_id, group_name, username, NULL, 1, created_at, created_at FROM subscribers;
            "#,
            [],
        );

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // --- Управление справочником групп МАИ ---

    pub async fn save_groups(&self, groups: &[GroupInfo]) -> Result<()> {
        let now = current_timestamp();
        let mut conn = self.conn.lock().await;
        let tx = conn.transaction()?;

        {
            let mut stmt = tx.prepare(
                r#"
                INSERT INTO mai_groups (name, name_lower, fac, level, course, updated_at)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                ON CONFLICT(name) DO UPDATE SET
                    name_lower = excluded.name_lower,
                    fac = excluded.fac,
                    level = excluded.level,
                    course = excluded.course,
                    updated_at = excluded.updated_at
                "#,
            )?;

            for g in groups {
                let clean_name = g.name.trim();
                if clean_name.is_empty() {
                    continue;
                }
                let lower_name = clean_name.to_lowercase();
                stmt.execute(params![
                    clean_name,
                    lower_name,
                    g.fac.trim(),
                    g.level.trim(),
                    g.course.trim(),
                    now
                ])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    pub async fn get_all_groups(&self) -> Result<Vec<GroupInfo>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT name, fac, level, course FROM mai_groups ORDER BY fac ASC, course ASC, name ASC",
        )?;
        let mut rows = stmt.query([])?;

        let mut list = Vec::new();
        while let Some(row) = rows.next()? {
            list.push(GroupInfo {
                name: row.get(0)?,
                fac: row.get(1)?,
                level: row.get(2)?,
                course: row.get(3)?,
            });
        }
        Ok(list)
    }

    pub async fn search_groups(&self, query: &str, limit: usize) -> Result<Vec<GroupInfo>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }

        let lower = trimmed.to_lowercase();
        let pattern = format!("%{}%", lower);

        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            r#"
            SELECT name, fac, level, course FROM mai_groups
            WHERE name_lower LIKE ?1
            ORDER BY
                CASE WHEN name_lower = ?2 THEN 0
                     WHEN name_lower LIKE ?2 || '%' THEN 1
                     ELSE 2
                END,
                name ASC
            LIMIT ?3
            "#,
        )?;

        let mut rows = stmt.query(params![pattern, lower, limit as i64])?;
        let mut list = Vec::new();
        while let Some(row) = rows.next()? {
            list.push(GroupInfo {
                name: row.get(0)?,
                fac: row.get(1)?,
                level: row.get(2)?,
                course: row.get(3)?,
            });
        }
        Ok(list)
    }

    pub async fn find_group_exact_or_ci(&self, query: &str) -> Result<Option<String>> {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let lower = trimmed.to_lowercase();
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT name FROM mai_groups WHERE name = ?1 OR name_lower = ?2 LIMIT 1",
        )?;
        let mut rows = stmt.query(params![trimmed, lower])?;

        if let Some(row) = rows.next()? {
            let official_name: String = row.get(0)?;
            Ok(Some(official_name))
        } else {
            Ok(None)
        }
    }

    #[allow(dead_code)]
    pub async fn count_groups(&self) -> Result<usize> {
        let conn = self.conn.lock().await;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM mai_groups", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    // --- Управление пользователями (users) ---

    pub async fn upsert_user(
        &self,
        id: i64,
        group_name: Option<&str>,
        username: Option<&str>,
        first_name: Option<&str>,
    ) -> Result<()> {
        let now = current_timestamp();
        let conn = self.conn.lock().await;

        let clean_grp = group_name.map(|s| s.trim()).filter(|s| !s.is_empty());

        conn.execute(
            r#"
            INSERT INTO users (id, group_name, username, first_name, notifications_enabled, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)
            ON CONFLICT(id) DO UPDATE SET
                group_name = COALESCE(excluded.group_name, users.group_name),
                username = COALESCE(excluded.username, users.username),
                first_name = COALESCE(excluded.first_name, users.first_name),
                updated_at = excluded.updated_at
            "#,
            params![id, clean_grp, username, first_name, now],
        )?;

        // Синхронизация с legacy таблицей subscribers
        if let Some(grp) = clean_grp {
            conn.execute(
                r#"
                INSERT INTO subscribers (chat_id, group_name, username, created_at)
                VALUES (?1, ?2, ?3, ?4)
                ON CONFLICT(chat_id) DO UPDATE SET
                    group_name = excluded.group_name,
                    username = excluded.username,
                    created_at = excluded.created_at
                "#,
                params![id, grp, username, now],
            )?;
        }

        Ok(())
    }

    pub async fn set_user_group(&self, id: i64, group_name: &str) -> Result<()> {
        let now = current_timestamp();
        let clean_grp = group_name.trim();
        let conn = self.conn.lock().await;

        conn.execute(
            r#"
            INSERT INTO users (id, group_name, username, first_name, notifications_enabled, created_at, updated_at)
            VALUES (?1, ?2, NULL, NULL, 1, ?3, ?3)
            ON CONFLICT(id) DO UPDATE SET
                group_name = excluded.group_name,
                updated_at = excluded.updated_at
            "#,
            params![id, clean_grp, now],
        )?;

        // Синхронизация с legacy таблицей subscribers
        conn.execute(
            r#"
            INSERT INTO subscribers (chat_id, group_name, username, created_at)
            VALUES (?1, ?2, NULL, ?3)
            ON CONFLICT(chat_id) DO UPDATE SET
                group_name = excluded.group_name,
                created_at = excluded.created_at
            "#,
            params![id, clean_grp, now],
        )?;

        Ok(())
    }

    pub async fn get_user(&self, id: i64) -> Result<Option<User>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, group_name, username, first_name, notifications_enabled, created_at, updated_at FROM users WHERE id = ?",
        )?;
        let mut rows = stmt.query(params![id])?;

        if let Some(row) = rows.next()? {
            let notif_raw: i64 = row.get(4)?;
            Ok(Some(User {
                id: row.get(0)?,
                group_name: row.get(1)?,
                username: row.get(2)?,
                first_name: row.get(3)?,
                notifications_enabled: notif_raw != 0,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
            }))
        } else {
            Ok(None)
        }
    }

    pub async fn get_user_group(&self, id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT group_name FROM users WHERE id = ?")?;
        let mut rows = stmt.query(params![id])?;

        if let Some(row) = rows.next()? {
            let grp: Option<String> = row.get(0)?;
            Ok(grp.filter(|s| !s.trim().is_empty()))
        } else {
            // Fallback to legacy subscribers table
            let mut legacy_stmt = conn.prepare("SELECT group_name FROM subscribers WHERE chat_id = ?")?;
            let mut legacy_rows = legacy_stmt.query(params![id])?;
            if let Some(r) = legacy_rows.next()? {
                let grp: String = r.get(0)?;
                Ok(Some(grp))
            } else {
                Ok(None)
            }
        }
    }

    pub async fn toggle_user_notifications(&self, id: i64) -> Result<bool> {
        let now = current_timestamp();
        let conn = self.conn.lock().await;

        let current_notif: Option<i64> = conn
            .query_row(
                "SELECT notifications_enabled FROM users WHERE id = ?",
                params![id],
                |r| r.get(0),
            )
            .ok();

        let new_state = match current_notif {
            Some(1) => 0,
            _ => 1,
        };

        conn.execute(
            r#"
            INSERT INTO users (id, group_name, username, first_name, notifications_enabled, created_at, updated_at)
            VALUES (?1, NULL, NULL, NULL, ?2, ?3, ?3)
            ON CONFLICT(id) DO UPDATE SET
                notifications_enabled = excluded.notifications_enabled,
                updated_at = excluded.updated_at
            "#,
            params![id, new_state, now],
        )?;

        Ok(new_state == 1)
    }

    #[allow(dead_code)]
    pub async fn count_users(&self) -> Result<usize> {
        let conn = self.conn.lock().await;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
        Ok(count as usize)
    }

    // --- Legacy методы для совместимости ---

    #[allow(dead_code)]
    pub async fn add_subscriber(&self, chat_id: i64, group_name: &str, username: Option<&str>) -> Result<()> {
        self.upsert_user(chat_id, Some(group_name), username, None).await
    }

    #[allow(dead_code)]
    pub async fn get_subscriber_group(&self, chat_id: i64) -> Result<Option<String>> {
        self.get_user_group(chat_id).await
    }


    pub async fn get_subscribers_for_group(&self, group_name: &str) -> Result<Vec<i64>> {
        let clean_grp = group_name.trim();
        let conn = self.conn.lock().await;

        // Таблица users является основным источником правды для подписчиков и их настроек уведомлений
        let mut stmt = conn.prepare(
            r#"
            SELECT id FROM users WHERE group_name = ?1 AND notifications_enabled = 1
            UNION
            SELECT chat_id FROM subscribers WHERE group_name = ?1 AND chat_id NOT IN (SELECT id FROM users)
            "#,
        )?;
        let mut rows = stmt.query(params![clean_grp])?;

        let mut list = Vec::new();
        while let Some(row) = rows.next()? {
            list.push(row.get(0)?);
        }

        Ok(list)
    }


    pub async fn get_all_active_groups(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            r#"
            SELECT DISTINCT group_name FROM users WHERE group_name IS NOT NULL AND TRIM(group_name) != ''
            UNION
            SELECT DISTINCT group_name FROM subscribers WHERE TRIM(group_name) != ''
            "#,
        )?;
        let mut rows = stmt.query([])?;

        let mut list = Vec::new();
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            if !name.trim().is_empty() {
                list.push(name);
            }
        }
        Ok(list)
    }

    #[allow(dead_code)]
    pub async fn get_all_monitored_groups(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            r#"
            SELECT DISTINCT group_name FROM users WHERE group_name IS NOT NULL AND TRIM(group_name) != ''
            UNION
            SELECT DISTINCT group_name FROM subscribers WHERE TRIM(group_name) != ''
            UNION
            SELECT DISTINCT group_name FROM schedule_snapshots
            "#,
        )?;
        let mut rows = stmt.query([])?;

        let mut list = Vec::new();
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            if !name.trim().is_empty() {
                list.push(name);
            }
        }
        Ok(list)
    }

    // --- Снимки расписаний (schedule_snapshots) ---

    pub async fn get_snapshot(&self, group_name: &str) -> Result<Option<GroupSchedule>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT schedule_json FROM schedule_snapshots WHERE group_name = ?")?;
        let mut rows = stmt.query(params![group_name.trim()])?;

        if let Some(row) = rows.next()? {
            let json_str: String = row.get(0)?;
            let schedule: GroupSchedule = serde_json::from_str(&json_str)?;
            Ok(Some(schedule))
        } else {
            Ok(None)
        }
    }

    pub async fn save_snapshot(&self, schedule: &GroupSchedule) -> Result<()> {
        let json_str = serde_json::to_string(schedule)?;
        let now = current_timestamp();
        let conn = self.conn.lock().await;

        conn.execute(
            r#"
            INSERT INTO schedule_snapshots (group_name, schedule_json, updated_at)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(group_name) DO UPDATE SET
                schedule_json = excluded.schedule_json,
                updated_at = excluded.updated_at
            "#,
            params![schedule.group.trim(), json_str, now],
        )?;

        Ok(())
    }

    // --- История изменений (change_history) ---

    pub async fn log_change(&self, group_name: &str, change_type: &str, description: &str) -> Result<()> {
        let now = current_timestamp();
        let conn = self.conn.lock().await;

        conn.execute(
            "INSERT INTO change_history (group_name, change_type, description, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![group_name.trim(), change_type, description, now],
        )?;

        Ok(())
    }

    pub async fn get_recent_changes(&self, group_name: &str, limit: usize) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT description, created_at FROM change_history WHERE group_name = ? ORDER BY id DESC LIMIT ?",
        )?;
        let mut rows = stmt.query(params![group_name.trim(), limit as i64])?;

        let mut list = Vec::new();
        while let Some(row) = rows.next()? {
            let desc: String = row.get(0)?;
            let created: String = row.get(1)?;
            let date_prefix = if created.len() >= 16 {
                &created[..16]
            } else {
                &created
            };
            list.push(format!("[{}] {}", date_prefix, desc));
        }
        Ok(list)
    }

    pub async fn get_all_changes(&self, limit: usize) -> Result<Vec<ChangeRecord>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id, group_name, change_type, description, created_at FROM change_history ORDER BY id DESC LIMIT ?",
        )?;
        let mut rows = stmt.query(params![limit as i64])?;

        let mut list = Vec::new();
        while let Some(row) = rows.next()? {
            list.push(ChangeRecord {
                id: row.get(0)?,
                group_name: row.get(1)?,
                change_type: row.get(2)?,
                description: row.get(3)?,
                created_at: row.get(4)?,
            });
        }
        Ok(list)
    }

    // --- Системные метаданные приложения ---

    pub async fn get_metadata(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT value FROM app_metadata WHERE key = ?1")?;
        let mut rows = stmt.query([key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    }

    pub async fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO app_metadata (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub async fn get_last_all_groups_sync(&self) -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        if let Some(val) = self.get_metadata("last_all_groups_sync").await? {
            if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&val) {
                return Ok(Some(dt.with_timezone(&chrono::Utc)));
            }
        }

        // Fallback: проверяем самую свежую дату в schedule_snapshots, если метаданные еще не записаны
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT max(updated_at) FROM schedule_snapshots")?;
        let mut rows = stmt.query([])?;
        if let Some(row) = rows.next()? {
            let max_dt: Option<String> = row.get(0)?;
            if let Some(dt_str) = max_dt {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&dt_str) {
                    return Ok(Some(dt.with_timezone(&chrono::Utc)));
                }
            }
        }
        Ok(None)
    }

    pub async fn set_last_all_groups_sync(&self, dt: chrono::DateTime<chrono::Utc>) -> Result<()> {
        self.set_metadata("last_all_groups_sync", &dt.to_rfc3339()).await
    }

    pub async fn checkpoint(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute_batch(
            r#"
            PRAGMA wal_checkpoint(TRUNCATE);
            PRAGMA optimize;
            "#,
        )?;
        Ok(())
    }
}


pub fn generate_changes_log_content(changes: &[ChangeRecord]) -> String {
    let now = Utc::now().format("%Y-%m-%d %H:%M:%S UTC").to_string();
    let mut out = String::new();
    out.push_str("=== ЛОГ ОБНОВЛЕНИЙ РАСПИСАНИЯ МАИ ===\n");
    out.push_str(&format!("Сгенерировано: {}\n", now));
    out.push_str(&format!("Всего записей: {}\n", changes.len()));
    out.push_str("--------------------------------------------------------------------------------\n");

    if changes.is_empty() {
        out.push_str("Изменений в расписании пока не зафиксировано.\n");
    } else {
        for c in changes {
            let date = if c.created_at.len() >= 19 {
                &c.created_at[..19]
            } else {
                &c.created_at
            };
            out.push_str(&format!(
                "[{}] [{}] [{}] {}\n",
                date, c.group_name, c.change_type, c.description
            ));
        }
    }
    out.push_str("--------------------------------------------------------------------------------\n");
    out
}


fn current_timestamp() -> String {
    Utc::now().to_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DaySchedule, Lesson};
    use chrono::NaiveDate;
    use std::collections::BTreeMap;

    fn dummy_schedule(group: &str) -> GroupSchedule {
        let date = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let mut days = BTreeMap::new();
        days.insert(
            "2026-09-03".to_string(),
            DaySchedule {
                date,
                day_of_week: "Чт".to_string(),
                lessons: vec![Lesson {
                    subject: "Физика".to_string(),
                    time_start: "09:00:00".to_string(),
                    time_end: "10:30:00".to_string(),
                    rooms: vec!["101".to_string()],
                    lectors: vec!["Иванов".to_string()],
                    lesson_types: vec!["ЛК".to_string()],
                    lms: None,
                    teams: None,
                    other: None,
                }],
            },
        );
        GroupSchedule {
            group: group.to_string(),
            days,
        }
    }

    #[tokio::test]
    async fn test_db_snapshot_operations() {
        let db = Database::new(":memory:").expect("Failed to create in-memory db");
        let sched = dummy_schedule("М14О-101БВ-26");

        assert!(db.get_snapshot("М14О-101БВ-26").await.unwrap().is_none());

        db.save_snapshot(&sched).await.unwrap();
        let loaded = db.get_snapshot("М14О-101БВ-26").await.unwrap();
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap().group, "М14О-101БВ-26");
    }

    #[tokio::test]
    async fn test_db_subscribers() {
        let db = Database::new(":memory:").expect("Failed to create in-memory db");
        assert!(db.get_subscriber_group(12345).await.unwrap().is_none());

        db.add_subscriber(12345, "М14О-101БВ-26", Some("user1")).await.unwrap();
        db.add_subscriber(67890, "М14О-101БВ-26", None).await.unwrap();
        db.add_subscriber(99999, "ДРУГАЯ-ГРУППА", None).await.unwrap();

        assert_eq!(
            db.get_subscriber_group(12345).await.unwrap().as_deref(),
            Some("М14О-101БВ-26")
        );

        let subs = db.get_subscribers_for_group("М14О-101БВ-26").await.unwrap();
        assert_eq!(subs.len(), 2);
        assert!(subs.contains(&12345));
        assert!(subs.contains(&67890));

        let monitored = db.get_all_monitored_groups().await.unwrap();
        assert_eq!(monitored.len(), 2);
        assert!(monitored.contains(&"М14О-101БВ-26".to_string()));
        assert!(monitored.contains(&"ДРУГАЯ-ГРУППА".to_string()));
    }

    #[tokio::test]
    async fn test_db_users_and_groups() {
        let db = Database::new(":memory:").expect("Failed to create in-memory db");

        // Groups
        let sample_groups = vec![
            GroupInfo {
                name: "М14О-101БВ-26".to_string(),
                fac: "Институт 1".to_string(),
                level: "Бакалавриат".to_string(),
                course: "1".to_string(),
            },
            GroupInfo {
                name: "М3О-309БВ-24".to_string(),
                fac: "Институт 3".to_string(),
                level: "Бакалавриат".to_string(),
                course: "3".to_string(),
            },
        ];

        db.save_groups(&sample_groups).await.unwrap();
        assert_eq!(db.count_groups().await.unwrap(), 2);

        let exact = db.find_group_exact_or_ci("м3о-309бв-24").await.unwrap();
        assert_eq!(exact.as_deref(), Some("М3О-309БВ-24"));

        let search = db.search_groups("309", 5).await.unwrap();
        assert_eq!(search.len(), 1);
        assert_eq!(search[0].name, "М3О-309БВ-24");

        // Users
        db.upsert_user(111, Some("М3О-309БВ-24"), Some("alice"), Some("Алиса")).await.unwrap();
        let u = db.get_user(111).await.unwrap().expect("User should exist");
        assert_eq!(u.group_name.as_deref(), Some("М3О-309БВ-24"));
        assert_eq!(u.first_name.as_deref(), Some("Алиса"));
        assert!(u.notifications_enabled);

        let is_now_enabled = db.toggle_user_notifications(111).await.unwrap();
        assert!(!is_now_enabled);

        let subs = db.get_subscribers_for_group("М3О-309БВ-24").await.unwrap();
        assert!(subs.is_empty()); // disabled

        let is_now_enabled = db.toggle_user_notifications(111).await.unwrap();
        assert!(is_now_enabled);
        let subs = db.get_subscribers_for_group("М3О-309БВ-24").await.unwrap();
        assert_eq!(subs, vec![111]);
    }

    #[tokio::test]
    async fn test_db_change_history() {
        let db = Database::new(":memory:").expect("Failed to create in-memory db");
        db.log_change("М14О-101БВ-26", "RoomChanged", "Аудитория изменена на 202").await.unwrap();
        db.log_change("М14О-101БВ-26", "Cancelled", "Занятие отменено").await.unwrap();

        let changes = db.get_recent_changes("М14О-101БВ-26", 5).await.unwrap();
        assert_eq!(changes.len(), 2);
        assert!(changes[0].contains("Занятие отменено"));
        assert!(changes[1].contains("Аудитория изменена на 202"));

        let all_changes = db.get_all_changes(10).await.unwrap();
        assert_eq!(all_changes.len(), 2);
        let file_content = generate_changes_log_content(&all_changes);
        assert!(file_content.contains("ЛОГ ОБНОВЛЕНИЙ РАСПИСАНИЯ МАИ"));
        assert!(file_content.contains("М14О-101БВ-26"));
        assert!(file_content.contains("RoomChanged"));
    }

    #[tokio::test]
    async fn test_db_metadata() {
        let db = Database::new(":memory:").expect("Failed to create in-memory db");
        assert!(db.get_metadata("test_key").await.unwrap().is_none());
        assert!(db.get_last_all_groups_sync().await.unwrap().is_none());

        db.set_metadata("test_key", "test_val").await.unwrap();
        assert_eq!(db.get_metadata("test_key").await.unwrap().as_deref(), Some("test_val"));

        let now = Utc::now();
        db.set_last_all_groups_sync(now).await.unwrap();
        let loaded = db.get_last_all_groups_sync().await.unwrap().expect("Should have sync time");
        assert_eq!(loaded.timestamp(), now.timestamp());
    }
}

