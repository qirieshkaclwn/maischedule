use anyhow::{Context, Result};
use chrono::Utc;
use rusqlite::params;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::models::GroupSchedule;

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
            CREATE INDEX IF NOT EXISTS idx_subscribers_group ON subscribers(group_name);
            CREATE INDEX IF NOT EXISTS idx_change_history_group_id ON change_history(group_name, id DESC);
            "#,
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

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

    pub async fn add_subscriber(&self, chat_id: i64, group_name: &str, username: Option<&str>) -> Result<()> {
        let now = current_timestamp();
        let conn = self.conn.lock().await;

        conn.execute(
            r#"
            INSERT INTO subscribers (chat_id, group_name, username, created_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(chat_id) DO UPDATE SET
                group_name = excluded.group_name,
                username = excluded.username,
                created_at = excluded.created_at
            "#,
            params![chat_id, group_name.trim(), username, now],
        )?;

        Ok(())
    }

    pub async fn get_subscriber_group(&self, chat_id: i64) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT group_name FROM subscribers WHERE chat_id = ?")?;
        let mut rows = stmt.query(params![chat_id])?;

        if let Some(row) = rows.next()? {
            let grp: String = row.get(0)?;
            Ok(Some(grp))
        } else {
            Ok(None)
        }
    }

    pub async fn get_subscribers_for_group(&self, group_name: &str) -> Result<Vec<i64>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare("SELECT chat_id FROM subscribers WHERE group_name = ?")?;
        let mut rows = stmt.query(params![group_name.trim()])?;

        let mut list = Vec::new();
        while let Some(row) = rows.next()? {
            list.push(row.get(0)?);
        }
        Ok(list)
    }

    pub async fn get_all_monitored_groups(&self) -> Result<Vec<String>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            r#"
            SELECT DISTINCT group_name FROM subscribers
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
    async fn test_db_change_history() {
        let db = Database::new(":memory:").expect("Failed to create in-memory db");
        db.log_change("М14О-101БВ-26", "RoomChanged", "Аудитория изменена на 202").await.unwrap();
        db.log_change("М14О-101БВ-26", "Cancelled", "Занятие отменено").await.unwrap();

        let changes = db.get_recent_changes("М14О-101БВ-26", 5).await.unwrap();
        assert_eq!(changes.len(), 2);
        // Latest change should be first (ORDER BY id DESC)
        assert!(changes[0].contains("Занятие отменено"));
        assert!(changes[1].contains("Аудитория изменена на 202"));
    }
}
