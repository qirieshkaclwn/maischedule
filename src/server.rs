use std::collections::HashMap;
use axum::{
    extract::{Path as AxumPath, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use tracing::error;

use crate::api::fetch_schedule;
use crate::calendar::generate_ical;
use crate::config::Config;
use crate::db::Database;
use crate::models::GroupInfo;
use crate::telegram::get_webcal_links;
use crate::utils::{escape_html, url_decode};

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    pub config: Config,
    pub client: reqwest::Client,
}

pub fn create_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/api/groups", get(groups_handler))
        .route("/api/schedule/:group", get(schedule_json_handler))
        .route("/api/user/:id", get(user_info_handler))
        .route("/api/user/group", post(set_user_group_handler))
        .route("/api/user/notifications", post(toggle_user_notifications_handler))
        .route("/calendar/:group", get(calendar_handler))
        .route("/webcal/:group", get(calendar_handler))
        .route("/subscribe/:group", get(subscribe_handler))
        .route("/app", get(miniapp_handler))
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

async fn groups_handler(State(state): State<AppState>) -> impl IntoResponse {
    match state.db.get_all_groups().await {
        Ok(groups) => Json(groups).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Ошибка получения списка групп: {}", e),
        )
            .into_response(),
    }
}

async fn calendar_handler(
    AxumPath(group_param): AxumPath<String>,
    State(state): State<AppState>,
) -> Response {
    let raw = group_param.trim_end_matches(".ics").trim();
    let clean_group = url_decode(raw);
    if clean_group.is_empty() {
        return (StatusCode::BAD_REQUEST, "Имя группы не может быть пустым").into_response();
    }

    // Сначала ищем в кэше БД, если нет - загружаем по API
    let schedule = match state.db.get_snapshot(&clean_group).await {
        Ok(Some(s)) => s,
        _ => match fetch_schedule(&state.client, &clean_group).await {
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

async fn subscribe_handler(
    AxumPath(group_param): AxumPath<String>,
    State(state): State<AppState>,
) -> Html<String> {
    let raw = group_param.trim_end_matches(".ics").trim();
    let clean_group = url_decode(raw);
    let all_groups = state.db.get_all_groups().await.unwrap_or_default();
    Html(render_portal_html(&state.config, &clean_group, &all_groups))
}

async fn index_handler(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Html<String> {
    let selected_grp = params
        .get("group")
        .cloned()
        .unwrap_or_else(|| state.config.default_group.clone());
    let all_groups = state.db.get_all_groups().await.unwrap_or_default();
    Html(render_portal_html(&state.config, &selected_grp, &all_groups))
}

async fn miniapp_handler(
    Query(params): Query<HashMap<String, String>>,
    State(state): State<AppState>,
) -> Html<String> {
    let group = params.get("group").cloned();
    Html(crate::miniapp::render_miniapp_html(&state.config, group.as_deref()))
}

async fn schedule_json_handler(
    AxumPath(group_param): AxumPath<String>,
    State(state): State<AppState>,
) -> Response {
    let raw = group_param.trim_end_matches(".ics").trim();
    let clean_group = url_decode(raw);
    if clean_group.is_empty() {
        return (StatusCode::BAD_REQUEST, "Имя группы не может быть пустым").into_response();
    }

    let schedule = match state.db.get_snapshot(&clean_group).await {
        Ok(Some(s)) => s,
        _ => match fetch_schedule(&state.client, &clean_group).await {
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

    Json(schedule).into_response()
}

async fn user_info_handler(
    AxumPath(user_id): AxumPath<i64>,
    State(state): State<AppState>,
) -> Response {
    match state.db.get_user(user_id).await {
        Ok(Some(user)) => Json(json!({
            "id": user.id,
            "group_name": user.group_name,
            "notifications_enabled": user.notifications_enabled,
        }))
        .into_response(),
        Ok(None) => Json(json!({
            "id": user_id,
            "group_name": null,
            "notifications_enabled": true,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Ошибка получения данных пользователя: {}", e),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
pub struct SetUserGroupPayload {
    pub user_id: i64,
    pub group_name: String,
    pub username: Option<String>,
    pub first_name: Option<String>,
}

async fn set_user_group_handler(
    State(state): State<AppState>,
    Json(payload): Json<SetUserGroupPayload>,
) -> Response {
    let clean_grp = payload.group_name.trim();
    if clean_grp.is_empty() {
        return (StatusCode::BAD_REQUEST, "Имя группы не может быть пустым").into_response();
    }

    if let Err(e) = state
        .db
        .upsert_user(
            payload.user_id,
            Some(clean_grp),
            payload.username.as_deref(),
            payload.first_name.as_deref(),
        )
        .await
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Ошибка сохранения пользователя: {}", e),
        )
            .into_response();
    }

    if let Err(e) = state.db.set_user_group(payload.user_id, clean_grp).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Ошибка установки группы: {}", e),
        )
            .into_response();
    }

    Json(json!({
        "status": "ok",
        "user_id": payload.user_id,
        "group_name": clean_grp,
    }))
    .into_response()
}

#[derive(serde::Deserialize)]
pub struct ToggleNotificationsPayload {
    pub user_id: i64,
}

async fn toggle_user_notifications_handler(
    State(state): State<AppState>,
    Json(payload): Json<ToggleNotificationsPayload>,
) -> Response {
    match state.db.toggle_user_notifications(payload.user_id).await {
        Ok(new_state) => Json(json!({
            "status": "ok",
            "user_id": payload.user_id,
            "notifications_enabled": new_state,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Ошибка переключения уведомлений: {}", e),
        )
            .into_response(),
    }
}

const PORTAL_TEMPLATE: &str = include_str!("../templates/portal.html");

fn render_portal_html(config: &Config, initial_group: &str, groups: &[GroupInfo]) -> String {
    let base = config.base_url.trim_end_matches('/');
    let (webcal_link, https_link) = get_webcal_links(config, initial_group);

    let mut options_html = String::new();
    for g in groups {
        options_html.push_str(&format!(
            r#"<option value="{}">{} (курс {})</option>"#,
            escape_html(&g.name),
            escape_html(&g.name),
            escape_html(&g.course)
        ));
    }

    PORTAL_TEMPLATE
        .replace("__BASE_URL__", base)
        .replace("__CURRENT_GROUP__", &escape_html(initial_group))
        .replace("__OPTIONS_HTML__", &options_html)
        .replace("__WEBCAL_LINK__", &webcal_link)
        .replace("__HTTPS_LINK__", &https_link)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_health_handler() {
        let response = health_handler().await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_index_handler() {
        let config = Config {
            bot_token: "".to_string(),
            telegram_chat_id: None,
            default_group: "М14О-101БВ-26".to_string(),
            host: "0.0.0.0".to_string(),
            port: 8000,
            base_url: "http://localhost:8000".to_string(),
            check_interval_minutes: 30,
            all_groups_sync_hours: 12,
            db_path: ":memory:".to_string(),
            timezone: "Europe/Moscow".to_string(),
            alert_minutes_before: 15,
        };
        let db = Database::new(":memory:").unwrap();
        let client = reqwest::Client::new();
        let state = AppState { db, config, client };

        let params = HashMap::new();
        let html_response = index_handler(Query(params), State(state)).await;
        let body = html_response.0;
        assert!(body.contains("Расписание МАИ"));
        assert!(body.contains("М14О-101БВ-26"));
        assert!(body.contains("webcal://"));
    }

    #[tokio::test]
    async fn test_groups_handler() {
        let config = Config {
            bot_token: "".to_string(),
            telegram_chat_id: None,
            default_group: "М14О-101БВ-26".to_string(),
            host: "0.0.0.0".to_string(),
            port: 8000,
            base_url: "http://localhost:8000".to_string(),
            check_interval_minutes: 30,
            all_groups_sync_hours: 12,
            db_path: ":memory:".to_string(),
            timezone: "Europe/Moscow".to_string(),
            alert_minutes_before: 15,
        };
        let db = Database::new(":memory:").unwrap();
        let client = reqwest::Client::new();
        let state = AppState { db, config, client };

        let response = groups_handler(State(state)).await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_miniapp_handler() {
        let config = Config {
            bot_token: "".to_string(),
            telegram_chat_id: None,
            default_group: "М14О-101БВ-26".to_string(),
            host: "0.0.0.0".to_string(),
            port: 8000,
            base_url: "http://localhost:8000".to_string(),
            check_interval_minutes: 30,
            all_groups_sync_hours: 12,
            db_path: ":memory:".to_string(),
            timezone: "Europe/Moscow".to_string(),
            alert_minutes_before: 15,
        };
        let db = Database::new(":memory:").unwrap();
        let client = reqwest::Client::new();
        let state = AppState { db, config, client };

        let mut params = HashMap::new();
        params.insert("group".to_string(), "М14О-101БВ-26".to_string());
        let html_response = miniapp_handler(Query(params), State(state)).await;
        let body = html_response.0;
        assert!(body.contains("telegram-web-app.js"));
        assert!(body.contains("М14О-101БВ-26"));
    }

    #[tokio::test]
    async fn test_user_api_handlers() {
        let config = Config {
            bot_token: "".to_string(),
            telegram_chat_id: None,
            default_group: "М14О-101БВ-26".to_string(),
            host: "0.0.0.0".to_string(),
            port: 8000,
            base_url: "http://localhost:8000".to_string(),
            check_interval_minutes: 30,
            all_groups_sync_hours: 12,
            db_path: ":memory:".to_string(),
            timezone: "Europe/Moscow".to_string(),
            alert_minutes_before: 15,
        };
        let db = Database::new(":memory:").unwrap();
        let client = reqwest::Client::new();
        let state = AppState { db, config, client };

        // 1. Set user group
        let payload = SetUserGroupPayload {
            user_id: 12345,
            group_name: "М14О-101БВ-26".to_string(),
            username: Some("testuser".to_string()),
            first_name: Some("Test".to_string()),
        };
        let set_res = set_user_group_handler(State(state.clone()), Json(payload)).await;
        assert_eq!(set_res.status(), StatusCode::OK);

        // 2. Get user info
        let info_res = user_info_handler(AxumPath(12345), State(state.clone())).await;
        assert_eq!(info_res.status(), StatusCode::OK);

        // 3. Toggle notifications
        let notif_res = toggle_user_notifications_handler(
            State(state.clone()),
            Json(ToggleNotificationsPayload { user_id: 12345 }),
        ).await;
        assert_eq!(notif_res.status(), StatusCode::OK);
    }
}

