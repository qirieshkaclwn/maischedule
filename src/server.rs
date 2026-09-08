use axum::{
    extract::{Path as AxumPath, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde_json::json;
use std::sync::Arc;
use tracing::{error, info};

use crate::api::fetch_schedule;
use crate::calendar::generate_ical;
use crate::config::Config;
use crate::db::Database;
use crate::telegram::get_webcal_links;

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub config: Config,
    pub client: reqwest::Client,
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/calendar/:group", get(calendar_handler))
        .route("/webcal/:group", get(calendar_handler))
        .route("/", get(index_handler))
        .with_state(state)
}

async fn health_handler() -> impl IntoResponse {
    Json(json!({
        "status": "ok",
        "engine": "rust",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn calendar_handler(
    AxumPath(group_param): AxumPath<String>,
    State(state): State<AppState>,
) -> Response {
    let clean_group = group_param.trim_end_matches(".ics").trim();
    if clean_group.is_empty() {
        return (StatusCode::BAD_REQUEST, "Имя группы не может быть пустым").into_response();
    }

    // Сначала ищем в кэше БД, если нет — загружаем по API
    let schedule = match state.db.get_snapshot(clean_group).await {
        Ok(Some(s)) => s,
        _ => match fetch_schedule(&state.client, clean_group).await {
            Ok(s) => {
                let _ = state.db.save_snapshot(&s).await;
                s
            }
            Err(e) => {
                error!("Не удалось получить расписание для {}: {:?}", clean_group, e);
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("Ошибка загрузки расписания МАИ: {}", e),
                )
                    .into_response();
            }
        },
    };

    let ics_content = generate_ical(
        &schedule,
        state.config.alert_minutes_before,
        &state.config.timezone,
    );

    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/calendar; charset=utf-8"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, no-store, must-revalidate"),
    );
    headers.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("inline; filename=\"schedule.ics\""),
    );

    (StatusCode::OK, headers, ics_content).into_response()
}

async fn index_handler(State(state): State<AppState>) -> Html<String> {
    let default_grp = &state.config.default_group;
    let (webcal_link, https_link) = get_webcal_links(&state.config, default_grp);

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="ru">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Синхронизация расписания МАИ с iPhone</title>
    <style>
        body {{
            font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
            background: #f2f2f7;
            color: #1c1c1e;
            margin: 0;
            padding: 20px;
            display: flex;
            justify-content: center;
        }}
        .card {{
            background: #ffffff;
            max-width: 540px;
            width: 100%;
            border-radius: 18px;
            padding: 28px;
            box-shadow: 0 4px 20px rgba(0,0,0,0.06);
        }}
        h1 {{
            font-size: 24px;
            margin-top: 0;
            color: #007aff;
            display: flex;
            align-items: center;
            gap: 10px;
        }}
        .badge {{
            display: inline-block;
            background: #e5f1ff;
            color: #007aff;
            padding: 4px 10px;
            border-radius: 8px;
            font-weight: 600;
            font-size: 14px;
        }}
        .button {{
            display: block;
            width: 100%;
            box-sizing: border-box;
            background: #007aff;
            color: #ffffff;
            text-align: center;
            padding: 14px 20px;
            border-radius: 12px;
            text-decoration: none;
            font-size: 16px;
            font-weight: 600;
            margin: 20px 0;
            transition: background 0.2s;
        }}
        .button:hover {{
            background: #0056b3;
        }}
        .instructions {{
            background: #f8f9fa;
            border-radius: 12px;
            padding: 16px;
            font-size: 14px;
            line-height: 1.5;
        }}
        .instructions ol {{
            margin: 0;
            padding-left: 20px;
        }}
        .link-box {{
            background: #e9ecef;
            padding: 10px 12px;
            border-radius: 8px;
            font-family: monospace;
            font-size: 13px;
            word-break: break-all;
            margin-top: 8px;
            user-select: all;
        }}
    </style>
</head>
<body>
    <div class="card">
        <h1>📅 Расписание МАИ (Rust)</h1>
        <p>Автоматическая синхронизация расписания с Apple Calendar на iPhone и Mac.</p>
        <p>Текущая группа: <span class="badge">{}</span></p>

        <a href="{}" class="button">📲 Добавить в Календарь на iPhone</a>

        <div class="instructions">
            <b>Как добавить календарь вручную на iPhone:</b>
            <ol>
                <li>Откройте <b>«Настройки»</b> на iPhone.</li>
                <li>Перейдите в <b>«Календарь»</b> ➡️ <b>«Учетные записи»</b>.</li>
                <li>Нажмите <b>«Добавить учетную запись»</b> ➡️ <b>«Другое»</b> ➡️ <b>«Подписной календарь»</b>.</li>
                <li>Вставьте ссылку на подписку:
                    <div class="link-box">{}</div>
                </li>
                <li>В параметрах установите <b>«Автообновление»</b>: <i>Каждые 15 минут</i>.</li>
            </ol>
        </div>
    </div>
</body>
</html>
"#,
        default_grp, webcal_link, https_link
    );

    Html(html)
}
