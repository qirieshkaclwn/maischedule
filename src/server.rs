use std::collections::HashMap;
use axum::{
    extract::{DefaultBodyLimit, Path as AxumPath, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{error, info};

use crate::api::fetch_schedule;
use crate::calendar::generate_ical;
use crate::config::Config;
use crate::db::{Database, ScheduleServerStatus};
use crate::models::{DaySchedule, GroupInfo};
use crate::telegram::get_webcal_links;
use crate::utils::{current_timestamp, escape_html, url_decode};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleApiResponse {
    pub group: String,
    pub days: std::collections::BTreeMap<String, DaySchedule>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub server_status: ScheduleServerStatus,
}

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
        .route("/static/telegram-web-app.js", get(telegram_web_app_js_handler))
        .route("/telegram-web-app.js", get(telegram_web_app_js_handler))
        .route("/app", get(miniapp_handler))
        .route("/", get(index_handler))
        .route(
            "/internal/ingest",
            post(ingest_handler).layer(DefaultBodyLimit::max(96 * 1024 * 1024)),
        )
        .with_state(state)
}

fn tokens_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Распаковывает tar.gz со снапшотом во временный каталог и атомарно подменяет им рабочий.
fn install_snapshot(dir: &std::path::Path, data: &[u8]) -> anyhow::Result<usize> {
    use std::io::Read;

    /// Потолок на один файл расписания: всё, что больше, — точно не расписание.
    const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

    let parent = dir
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    std::fs::create_dir_all(&parent)?;

    // Уникальный суффикс на каждый вызов: два параллельных приёма не должны делить каталоги
    let unique = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    let tmp = parent.join(format!(".snapshot-tmp-{}", unique));
    let old = parent.join(format!(".snapshot-old-{}", unique));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    std::fs::create_dir_all(&tmp)?;

    let unpacked = (|| -> anyhow::Result<usize> {
        let mut decoder = flate2::read::GzDecoder::new(data);
        let mut archive = tar::Archive::new(&mut decoder);
        let mut count = 0usize;

        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?.to_path_buf();
            let name = match path.file_name().and_then(|s| s.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            // Только плоские имена вида <md5>.json или groups.json — никаких путей и переходов вверх
            let valid = path.components().count() == 1
                && name.ends_with(".json")
                && name.len() <= 128
                && !name.starts_with('.');
            if !valid {
                continue;
            }

            let mut buf = Vec::new();
            entry.take(MAX_FILE_BYTES + 1).read_to_end(&mut buf)?;
            if buf.len() as u64 > MAX_FILE_BYTES {
                anyhow::bail!("файл {} больше {} МБ", name, MAX_FILE_BYTES / 1024 / 1024);
            }
            std::fs::write(tmp.join(&name), &buf)?;
            count += 1;
        }

        if count == 0 {
            anyhow::bail!("снапшот не содержит ни одного json-файла");
        }
        Ok(count)
    })();

    let count = match unpacked {
        Ok(c) => c,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            return Err(e);
        }
    };

    if old.exists() {
        std::fs::remove_dir_all(&old)?;
    }
    if dir.exists() {
        std::fs::rename(dir, &old)?;
    }
    std::fs::rename(&tmp, dir)?;
    if old.exists() {
        std::fs::remove_dir_all(&old)?;
    }
    Ok(count)
}

/// Приём снапшота расписания от сборщика, работающего внутри сети МАИ.
async fn ingest_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let expected = state.config.ingest_token.trim().to_string();
    if expected.is_empty() {
        return (StatusCode::NOT_FOUND, "ingest disabled").into_response();
    }

    let got = headers
        .get("x-ingest-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_string();
    if !tokens_equal(got.as_bytes(), expected.as_bytes()) {
        return (StatusCode::UNAUTHORIZED, "bad token").into_response();
    }

    let dir = state.config.snapshot_dir.trim().to_string();
    if dir.is_empty() {
        return (StatusCode::SERVICE_UNAVAILABLE, "SNAPSHOT_DIR не задан").into_response();
    }
    if body.is_empty() {
        return (StatusCode::BAD_REQUEST, "empty body").into_response();
    }

    let size = body.len();
    let path = std::path::PathBuf::from(dir);
    match tokio::task::spawn_blocking(move || install_snapshot(&path, &body)).await {
        Ok(Ok(files)) => {
            info!(files, bytes = size, "Снапшот расписания принят");
            (
                StatusCode::OK,
                Json(json!({"ok": true, "files": files, "bytes": size})),
            )
                .into_response()
        }
        Ok(Err(e)) => {
            error!("Не удалось установить снапшот: {:?}", e);
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("install failed: {}", e),
            )
                .into_response()
        }
        Err(e) => {
            error!("Паника при установке снапшота: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, "join error").into_response()
        }
    }
}

const TELEGRAM_WEB_APP_JS: &str = include_str!("../static/telegram-web-app.js");

async fn telegram_web_app_js_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (
                header::CACHE_CONTROL,
                "public, max-age=31536000, immutable",
            ),
        ],
        TELEGRAM_WEB_APP_JS,
    )
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
        Ok(groups) => (
            StatusCode::OK,
            [(header::CACHE_CONTROL, "public, max-age=3600, stale-while-revalidate=86400")],
            Json(groups),
        )
            .into_response(),
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

    let mut server_status = state.db.get_schedule_server_status().await.unwrap_or(ScheduleServerStatus {
        is_down: false,
        down_since: None,
        last_error: None,
        last_success: None,
    });

    let (schedule, updated_at) = match state.db.get_snapshot_with_meta(&clean_group).await {
        Ok(Some((s, dt))) => (s, Some(dt)),
        _ => match fetch_schedule(&state.client, &clean_group).await {
            Ok(s) => {
                let _ = state.db.record_schedule_server_success().await;
                let _ = state.db.save_snapshot(&s).await;
                let now = current_timestamp();
                server_status.is_down = false;
                server_status.last_success = Some(now.clone());
                (s, Some(now))
            }
            Err(e) => {
                let err_msg = e.to_string();
                let _ = state.db.record_schedule_server_failure(&err_msg).await;
                server_status.is_down = true;
                if server_status.down_since.is_none() {
                    server_status.down_since = Some(current_timestamp());
                }
                server_status.last_error = Some(err_msg.clone());

                error!("Не удалось получить расписание для {}: {:?}", clean_group, e);
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(json!({
                        "error": format!("Ошибка загрузки расписания МАИ: {}", err_msg),
                        "server_status": server_status,
                    })),
                )
                    .into_response();
            }
        },
    };

    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "public, max-age=60, stale-while-revalidate=300")],
        Json(ScheduleApiResponse {
            group: schedule.group,
            days: schedule.days,
            updated_at,
            server_status,
        }),
    )
    .into_response()
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
    fn make_snapshot_archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            for (name, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                builder.append_data(&mut header, name, *data).unwrap();
            }
            builder.finish().unwrap();
        }
        encoder.finish().unwrap()
    }

    #[test]
    fn install_snapshot_roundtrip() {
        let dir = std::env::temp_dir().join(format!("snap-ok-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let archive = make_snapshot_archive(&[
            ("groups.json", b"[{\"name\":\"X\"}]"),
            ("abc.json", b"{\"a\":1}"),
        ]);
        let files = install_snapshot(&dir, &archive).unwrap();

        assert_eq!(files, 2);
        assert!(dir.join("groups.json").exists());
        assert!(dir.join("abc.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_snapshot_ignores_foreign_paths() {
        let dir = std::env::temp_dir().join(format!("snap-skip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let archive = make_snapshot_archive(&[
            ("sub/nested.json", b"{}"),
            (".hidden.json", b"{}"),
            ("ok.json", b"{}"),
        ]);
        let files = install_snapshot(&dir, &archive).unwrap();

        assert_eq!(files, 1, "должен остаться только плоский ok.json");
        assert!(dir.join("ok.json").exists());
        assert!(!dir.join("sub").exists());
        assert!(!dir.join(".hidden.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn install_snapshot_rejects_empty_archive() {
        let dir = std::env::temp_dir().join(format!("snap-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let archive = make_snapshot_archive(&[]);
        assert!(install_snapshot(&dir, &archive).is_err());
    }

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
            snapshot_dir: String::new(),
            ingest_token: String::new(),
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
            snapshot_dir: String::new(),
            ingest_token: String::new(),
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
            snapshot_dir: String::new(),
            ingest_token: String::new(),
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
            snapshot_dir: String::new(),
            ingest_token: String::new(),
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

    #[tokio::test]
    async fn test_schedule_json_handler() {
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
            snapshot_dir: String::new(),
            ingest_token: String::new(),
        };
        let db = Database::new(":memory:").unwrap();
        let client = reqwest::Client::new();

        // Сохраняем тестовый снимок в БД
        let sched = crate::models::GroupSchedule {
            group: "М14О-101БВ-26".to_string(),
            days: std::collections::BTreeMap::new(),
        };
        db.save_snapshot(&sched).await.unwrap();

        let state = AppState { db, config, client };
        let res = schedule_json_handler(AxumPath("М14О-101БВ-26".to_string()), State(state)).await;
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_telegram_web_app_js_handler() {
        let response = telegram_web_app_js_handler().await.into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok()),
            Some("application/javascript; charset=utf-8")
        );
    }
}

