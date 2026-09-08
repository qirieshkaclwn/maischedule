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
        if let Some(parent) = Path::new(db_path).parent() {
            std::fs::create_dir_all(parent)?;
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

        // Создание таблиц
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
        let now = Utc::now().to_rfc3339();
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
        let now = Utc::now().to_rfc3339();
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
        let now = Utc::now().to_rfc3339();
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
