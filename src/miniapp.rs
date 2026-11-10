use crate::config::Config;

/// Renders the complete HTML, CSS and JavaScript for the Telegram Mini App.
pub fn render_miniapp_html(config: &Config, initial_group: Option<&str>) -> String {
    let base = config.base_url.trim_end_matches('/');
    let def_group = initial_group
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&config.default_group);

    MINIAPP_TEMPLATE
        .replace("__BASE_URL__", base)
        .replace("__DEFAULT_GROUP__", def_group)
}

const MINIAPP_TEMPLATE: &str = include_str!("../templates/miniapp.html");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_miniapp_html() {
        let config = Config {
            bot_token: "test_token".to_string(),
            telegram_chat_id: None,
            default_group: "М3О-101Б-24".to_string(),
            host: "127.0.0.1".to_string(),
            port: 8080,
            base_url: "https://schedule.example.com/".to_string(),
            check_interval_minutes: 30,
            all_groups_sync_hours: 24,
            db_path: ":memory:".to_string(),
            timezone: "Europe/Moscow".to_string(),
            alert_minutes_before: 15,
        };

        let html = render_miniapp_html(&config, None);
        assert!(html.contains("https://schedule.example.com"));
        assert!(html.contains("М3О-101Б-24"));
        assert!(html.contains("telegram-web-app.js"));

        let custom = render_miniapp_html(&config, Some("М3О-309БВ-24"));
        assert!(custom.contains("М3О-309БВ-24"));
    }
}
