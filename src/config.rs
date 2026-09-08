use std::env;

#[derive(Debug, Clone)]
pub struct Config {
    pub bot_token: String,
    pub telegram_chat_id: Option<i64>,
    pub default_group: String,
    pub host: String,
    pub port: u16,
    pub base_url: String,
    pub check_interval_minutes: u64,
    pub db_path: String,
    pub timezone: String,
    pub alert_minutes_before: u32,
}

impl Config {
    pub fn from_env() -> Self {
        // Загружаем .env, если есть
        let _ = dotenvy::dotenv();

        let bot_token = env::var("BOT_TOKEN").unwrap_or_default();
        let telegram_chat_id = env::var("TELEGRAM_CHAT_ID")
            .ok()
            .and_then(|s| s.parse::<i64>().ok());
        let default_group = env::var("DEFAULT_GROUP")
            .unwrap_or_else(|_| "М14О-101БВ-26".to_string());
        let host = env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = env::var("PORT")
            .ok()
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(8000);
        let base_url = env::var("BASE_URL")
            .unwrap_or_else(|_| "http://localhost:8000".to_string());
        let check_interval_minutes = env::var("CHECK_INTERVAL_MINUTES")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(30);
        let db_path = env::var("DB_PATH")
            .unwrap_or_else(|_| "data/maischedule.db".to_string());
        let timezone = env::var("TIMEZONE")
            .unwrap_or_else(|_| "Europe/Moscow".to_string());
        let alert_minutes_before = env::var("ALERT_MINUTES_BEFORE")
            .ok()
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(15);

        Self {
            bot_token,
            telegram_chat_id,
            default_group,
            host,
            port,
            base_url,
            check_interval_minutes,
            db_path,
            timezone,
            alert_minutes_before,
        }
    }
}
