use anyhow::{Context, Result};
use chrono::{Datelike, Local, NaiveDate};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::api::fetch_schedule;
use crate::config::Config;
use crate::db::Database;
use crate::diff::{detect_diff, format_diff_message};
use crate::models::{DaySchedule, GroupSchedule};
use crate::utils::escape_html;

#[derive(Clone)]
pub struct TelegramBot {
    pub token: String,
    pub client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
pub struct TelegramResponse<T> {
    pub ok: bool,
    pub result: Option<T>,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramUpdate {
    pub update_id: i64,
    pub message: Option<TelegramMessage>,
    pub callback_query: Option<TelegramCallbackQuery>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramMessage {
    #[allow(dead_code)]
    pub message_id: i64,
    pub chat: TelegramChat,
    pub from: Option<TelegramUser>,
    pub text: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramCallbackQuery {
    pub id: String,
    pub from: TelegramUser,
    pub message: Option<TelegramMessage>,
    pub data: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TelegramChat {
    pub id: i64,
}

#[derive(Debug, Deserialize)]
pub struct TelegramUser {
    #[allow(dead_code)]
    pub id: i64,
    #[allow(dead_code)]
    pub first_name: String,
    pub username: Option<String>,
}

impl TelegramBot {
    pub fn new(token: String, client: reqwest::Client) -> Self {
        Self { token, client }
    }

    fn api_url(&self, method: &str) -> String {
        format!("https://api.telegram.org/bot{}/{}", self.token, method)
    }

    pub async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        reply_markup: Option<serde_json::Value>,
    ) -> Result<()> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });

        if let Some(markup) = reply_markup {
            body.as_object_mut()
                .unwrap()
                .insert("reply_markup".to_string(), markup);
        }

        let resp = self
            .client
            .post(self.api_url("sendMessage"))
            .json(&body)
            .send()
            .await
            .context("Ошибка отправки sendMessage в Telegram")?;

        let res: TelegramResponse<serde_json::Value> = resp.json().await?;
        if !res.ok {
            warn!("Telegram API error: {:?}", res.description);
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub async fn edit_message_text(
        &self,
        chat_id: i64,
        message_id: i64,
        text: &str,
    ) -> Result<()> {
        let body = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });

        let resp = self
            .client
            .post(self.api_url("editMessageText"))
            .json(&body)
            .send()
            .await?;

        let res: TelegramResponse<serde_json::Value> = resp.json().await?;
        if !res.ok {
            warn!("Telegram API error in editMessageText: {:?}", res.description);
        }
        Ok(())
    }

    pub async fn answer_callback_query(&self, callback_id: &str) -> Result<()> {
        let body = json!({ "callback_query_id": callback_id });
        let _ = self
            .client
            .post(self.api_url("answerCallbackQuery"))
            .json(&body)
            .send()
            .await;
        Ok(())
    }

    pub async fn get_updates(
        &self,
        offset: Option<i64>,
        timeout: u32,
    ) -> Result<Vec<TelegramUpdate>> {
        let mut body = json!({
            "timeout": timeout,
            "allowed_updates": ["message", "callback_query"],
        });

        if let Some(off) = offset {
            body.as_object_mut()
                .unwrap()
                .insert("offset".to_string(), json!(off));
        }

        let resp = self
            .client
            .post(self.api_url("getUpdates"))
            .json(&body)
            .send()
            .await
            .context("Ошибка getUpdates в Telegram")?;

        let res: TelegramResponse<Vec<TelegramUpdate>> = resp.json().await?;
        if res.ok {
            Ok(res.result.unwrap_or_default())
        } else {
            anyhow::bail!("Ошибка getUpdates: {:?}", res.description);
        }
    }
}

pub fn get_webcal_links(config: &Config, group: &str) -> (String, String) {
    let base = config.base_url.trim_end_matches('/');
    let encoded = crate::utils::url_encode(group);
    let https = format!("{}/calendar/{}.ics", base, encoded);
    let webcal = https.replace("http://", "webcal://").replace("https://", "webcal://");
    (webcal, https)
}

pub fn get_subscribe_url(config: &Config, group: &str) -> String {
    let base = config.base_url.trim_end_matches('/');
    let encoded = crate::utils::url_encode(group);
    format!("{}/subscribe/{}", base, encoded)
}

pub fn main_keyboard(subscribe_url: &str) -> serde_json::Value {
    json!({
        "inline_keyboard": [
            [
                { "text": "Сегодня", "callback_data": "btn_today" },
                { "text": "Завтра", "callback_data": "btn_tomorrow" }
            ],
            [
                { "text": "Расписание на неделю", "callback_data": "btn_week" }
            ],
            [
                { "text": "Добавить в Календарь iPhone", "url": subscribe_url }
            ],
            [
                { "text": "Инструкция для iOS", "callback_data": "btn_link" },
                { "text": "Проверить обновления", "callback_data": "btn_check" }
            ]
        ]
    })
}

pub fn parse_command(text: &str) -> (&str, &str) {
    let trimmed = text.trim();
    let mut parts = trimmed.splitn(2, char::is_whitespace);
    let mut cmd = parts.next().unwrap_or("");
    if let Some((base, _botname)) = cmd.split_once('@') {
        cmd = base;
    }
    let args = parts.next().unwrap_or("").trim();
    (cmd, args)
}

pub fn format_day_schedule(day: Option<&DaySchedule>, target_date: &NaiveDate) -> String {
    let date_str = target_date.format("%d.%m.%Y").to_string();
    let day_name = day.map(|d| d.day_of_week.as_str()).unwrap_or("");

    let day = match day {
        Some(d) if !d.lessons.is_empty() => d,
        _ => {
            return format!(
                "<b>{} ({})</b>\n\nПар нет, можно отдыхать!",
                date_str,
                escape_html(day_name)
            );
        }
    };

    let mut out = format!(
        "<b>{} ({})</b>\n\n",
        date_str,
        escape_html(day_name)
    );
    for (idx, lesson) in day.lessons.iter().enumerate() {
        out.push_str(&format!(
            "<b>{}. {} – {}</b> [{}]\n   <b>{}</b>\n   Ауд: {}\n   Преподаватель: {}\n",
            idx + 1,
            lesson.time_start_clean(),
            lesson.time_end_clean(),
            escape_html(&lesson.type_str()),
            escape_html(&lesson.subject),
            escape_html(&lesson.room_str()),
            escape_html(&lesson.lector_str())
        ));
        if let Some(lms) = &lesson.lms {
            out.push_str(&format!("   LMS: {}\n", escape_html(lms)));
        }
        if let Some(teams) = &lesson.teams {
            out.push_str(&format!("   Teams: {}\n", escape_html(teams)));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

async fn get_or_load_schedule(
    group: &str,
    db: &Database,
    client: &reqwest::Client,
) -> Result<GroupSchedule> {
    if let Some(sched) = db.get_snapshot(group).await? {
        return Ok(sched);
    }
    let sched = fetch_schedule(client, group).await?;
    let _ = db.save_snapshot(&sched).await;
    Ok(sched)
}

pub struct BotContext<'a> {
    pub bot: &'a TelegramBot,
    pub db: &'a Database,
    pub config: &'a Config,
    pub client: &'a reqwest::Client,
}

pub async fn handle_command(
    text: &str,
    chat_id: i64,
    username: Option<&str>,
    ctx: &BotContext<'_>,
) -> Result<()> {
    let (cmd, args) = parse_command(text);
    let user_group = ctx
        .db
        .get_subscriber_group(chat_id)
        .await?
        .unwrap_or_else(|| ctx.config.default_group.clone());

    match cmd {
        "/start" => handle_start(chat_id, &user_group, username, ctx).await,
        "/help" => handle_help(chat_id, ctx).await,
        "/link" => handle_link(chat_id, &user_group, ctx).await,
        "/today" => handle_today(chat_id, &user_group, ctx).await,
        "/tomorrow" => handle_tomorrow(chat_id, &user_group, ctx).await,
        "/week" => handle_week(chat_id, &user_group, ctx).await,
        "/group" => handle_group(chat_id, &user_group, args, username, ctx).await,
        "/changes" => handle_changes(chat_id, &user_group, ctx).await,
        "/check" => handle_check(chat_id, &user_group, ctx).await,
        _ if cmd.starts_with('/') => {
            let msg = "Неизвестная команда. Введите /help, чтобы посмотреть список доступных команд.";
            ctx.bot.send_message(chat_id, msg, None).await
        }
        _ => Ok(()),
    }
}

async fn handle_start(
    chat_id: i64,
    user_group: &str,
    username: Option<&str>,
    ctx: &BotContext<'_>,
) -> Result<()> {
    ctx.db.add_subscriber(chat_id, user_group, username).await?;
    let subscribe_url = get_subscribe_url(ctx.config, user_group);
    let text = format!(
        "Привет!\n\n\
        Я бот на <b>Rust</b> для автосинхронизации расписания МАИ с твоим iPhone и уведомлений об изменениях.\n\n\
        Твоя группа: <b>{}</b>\n\n\
        <b>Как подключить календарь на iPhone:</b>\n\
        1. Нажми на кнопку <b>«Добавить в Календарь iPhone»</b> ниже.\n\
        2. iOS предложит подписаться на календарь — нажми <b>«Подписаться»</b>.\n\
        3. Включи <i>«Автообновление: Каждые 15 минут»</i>.\n\n\
        <i>Я пришлю сообщение, если пару перенесут, отменят или изменят аудиторию!</i>",
        escape_html(user_group)
    );
    ctx.bot.send_message(chat_id, &text, Some(main_keyboard(&subscribe_url))).await
}

async fn handle_help(chat_id: i64, ctx: &BotContext<'_>) -> Result<()> {
    let text = "<b>Доступные команды:</b>\n\n\
        /start — Главное меню и подключение календаря\n\
        /today — Расписание на сегодня\n\
        /tomorrow — Расписание на завтра\n\
        /week — Расписание на текущую неделю (Пн–Сб)\n\
        /group &lt;имя&gt; — Сменить учебную группу (напр. <code>/group М14О-101БВ-26</code>)\n\
        /link — Ссылки Webcal и HTTPS для календаря\n\
        /changes — История последних изменений\n\
        /check — Ручная проверка изменений прямо сейчас\n\
        /help — Справка по командам";
    ctx.bot.send_message(chat_id, text, None).await
}

async fn handle_link(
    chat_id: i64,
    user_group: &str,
    ctx: &BotContext<'_>,
) -> Result<()> {
    let (webcal_url, https_url) = get_webcal_links(ctx.config, user_group);
    let subscribe_url = get_subscribe_url(ctx.config, user_group);
    let text = format!(
        "<b>Ссылки для синхронизации группы {}:</b>\n\n\
        <b>Страница быстрого подключения:</b>\n\
        <code>{}</code>\n\n\
        <b>Webcal-ссылка:</b>\n\
        <code>{}</code>\n\n\
        <b>HTTPS-ссылка для ручного добавления:</b>\n\
        <code>{}</code>\n\n\
        <b>Инструкция для ручного добавления на iPhone:</b>\n\
        1. Настройки -> Календарь -> Учетные записи\n\
        2. Добавить учетную запись -> Другое -> Подписной календарь\n\
        3. Вставьте HTTPS-ссылку выше и сохраните.",
        escape_html(user_group),
        escape_html(&subscribe_url),
        escape_html(&webcal_url),
        escape_html(&https_url)
    );
    ctx.bot.send_message(chat_id, &text, None).await
}

async fn handle_today(
    chat_id: i64,
    user_group: &str,
    ctx: &BotContext<'_>,
) -> Result<()> {
    let today = Local::now().date_naive();
    match get_or_load_schedule(user_group, ctx.db, ctx.client).await {
        Ok(sched) => {
            let day = sched.get_day(&today);
            let text = format!(
                "Группа: <b>{}</b>\n\n{}",
                escape_html(user_group),
                format_day_schedule(day, &today)
            );
            ctx.bot.send_message(chat_id, &text, None).await
        }
        Err(e) => {
            ctx.bot
                .send_message(
                    chat_id,
                    &format!("Ошибка получения расписания: {}", escape_html(&e.to_string())),
                    None,
                )
                .await
        }
    }
}

async fn handle_tomorrow(
    chat_id: i64,
    user_group: &str,
    ctx: &BotContext<'_>,
) -> Result<()> {
    let tomorrow = Local::now().date_naive() + chrono::Duration::days(1);
    match get_or_load_schedule(user_group, ctx.db, ctx.client).await {
        Ok(sched) => {
            let day = sched.get_day(&tomorrow);
            let text = format!(
                "Группа: <b>{}</b>\n\n{}",
                escape_html(user_group),
                format_day_schedule(day, &tomorrow)
            );
            ctx.bot.send_message(chat_id, &text, None).await
        }
        Err(e) => {
            ctx.bot
                .send_message(
                    chat_id,
                    &format!("Ошибка получения расписания: {}", escape_html(&e.to_string())),
                    None,
                )
                .await
        }
    }
}

async fn handle_week(
    chat_id: i64,
    user_group: &str,
    ctx: &BotContext<'_>,
) -> Result<()> {
    let today = Local::now().date_naive();
    let weekday_num = today.weekday().num_days_from_monday();
    let monday = today - chrono::Duration::days(weekday_num as i64);

    match get_or_load_schedule(user_group, ctx.db, ctx.client).await {
        Ok(sched) => {
            let mut blocks = vec![format!(
                "<b>Расписание на неделю ({}):</b>\n",
                escape_html(user_group)
            )];
            for i in 0..6 {
                let d = monday + chrono::Duration::days(i);
                if let Some(ds) = sched.get_day(&d) {
                    if !ds.lessons.is_empty() {
                        blocks.push(format_day_schedule(Some(ds), &d));
                        blocks.push("----------------------------".to_string());
                    }
                }
            }
            if blocks.len() == 1 {
                blocks.push("Занятий на этой неделе не найдено.".to_string());
            }
            ctx.bot.send_message(chat_id, &blocks.join("\n\n"), None).await
        }
        Err(e) => {
            ctx.bot
                .send_message(
                    chat_id,
                    &format!("Ошибка: {}", escape_html(&e.to_string())),
                    None,
                )
                .await
        }
    }
}

async fn handle_group(
    chat_id: i64,
    user_group: &str,
    arg: &str,
    username: Option<&str>,
    ctx: &BotContext<'_>,
) -> Result<()> {
    if arg.is_empty() {
        let text = format!(
            "Текущая группа: <b>{}</b>\n\n\
            Чтобы изменить группу, отправьте команду:\n\
            <code>/group Название-Группы</code>\n\
            Например: <code>/group М14О-101БВ-26</code>",
            escape_html(user_group)
        );
        ctx.bot.send_message(chat_id, &text, None).await
    } else {
        let cleaned = crate::utils::strip_emojis(arg);
        let new_grp = cleaned.trim();
        match fetch_schedule(ctx.client, new_grp).await {
            Ok(sched) => {
                ctx.db.save_snapshot(&sched).await?;
                ctx.db.add_subscriber(chat_id, new_grp, username).await?;
                let new_subscribe = get_subscribe_url(ctx.config, new_grp);
                let text = format!(
                    "Группа успешно обновлена на <b>{}</b>!\n\n\
                    Не забудьте обновить ссылку в календаре на iPhone:",
                    escape_html(new_grp)
                );
                ctx.bot
                    .send_message(chat_id, &text, Some(main_keyboard(&new_subscribe)))
                    .await
            }
            Err(_) => {
                ctx.bot
                    .send_message(
                        chat_id,
                        &format!(
                            "Не удалось найти расписание для «{}». Проверьте написание.",
                            escape_html(new_grp)
                        ),
                        None,
                    )
                    .await
            }
        }
    }
}

async fn handle_changes(
    chat_id: i64,
    user_group: &str,
    ctx: &BotContext<'_>,
) -> Result<()> {
    match ctx.db.get_recent_changes(user_group, 10).await {
        Ok(hist) if hist.is_empty() => {
            ctx.bot
                .send_message(
                    chat_id,
                    &format!("Для группы <b>{}</b> изменений не зафиксировано.", escape_html(user_group)),
                    None,
                )
                .await
        }
        Ok(hist) => {
            let escaped_hist: Vec<String> = hist.into_iter().map(|h| escape_html(&h)).collect();
            let text = format!(
                "<b>Последние изменения в расписании ({}):</b>\n\n{}",
                escape_html(user_group),
                escaped_hist.join("\n")
            );
            ctx.bot.send_message(chat_id, &text, None).await
        }
        Err(e) => {
            ctx.bot
                .send_message(
                    chat_id,
                    &format!("Ошибка чтения истории: {}", escape_html(&e.to_string())),
                    None,
                )
                .await
        }
    }
}

async fn handle_check(
    chat_id: i64,
    user_group: &str,
    ctx: &BotContext<'_>,
) -> Result<()> {
    match fetch_schedule(ctx.client, user_group).await {
        Ok(new_sched) => {
            let old_sched = ctx.db.get_snapshot(user_group).await?;
            if let Some(old) = old_sched {
                let diff = detect_diff(&old, &new_sched);
                if diff.is_empty() {
                    ctx.bot
                        .send_message(
                            chat_id,
                            &format!("Расписание группы <b>{}</b> проверено — изменений нет.", escape_html(user_group)),
                            None,
                        )
                        .await
                } else {
                    for c in &diff {
                        let _ = ctx
                            .db
                            .log_change(user_group, &format!("{:?}", c.change_type), &c.details)
                            .await;
                    }
                    let _ = ctx.db.save_snapshot(&new_sched).await;
                    let msg = format_diff_message(user_group, &diff);
                    ctx.bot.send_message(chat_id, &msg, None).await
                }
            } else {
                let _ = ctx.db.save_snapshot(&new_sched).await;
                ctx.bot
                    .send_message(
                        chat_id,
                        &format!("Расписание группы <b>{}</b> сохранено. Изменений пока нет.", escape_html(user_group)),
                        None,
                    )
                    .await
            }
        }
        Err(e) => {
            ctx.bot
                .send_message(
                    chat_id,
                    &format!("Ошибка при проверке: {}", escape_html(&e.to_string())),
                    None,
                )
                .await
        }
    }
}

pub async fn run_polling(
    bot: TelegramBot,
    db: Database,
    config: Config,
    client: reqwest::Client,
    mut shutdown: watch::Receiver<bool>,
) {
    info!("Запуск поллинга Telegram-бота на Rust...");
    let mut offset = None;
    let ctx = BotContext {
        bot: &bot,
        db: &db,
        config: &config,
        client: &client,
    };

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                info!("Завершение работы поллинга Telegram-бота...");
                break;
            }
            res = bot.get_updates(offset, 25) => {
                match res {
                    Ok(updates) => {
                        for u in updates {
                            offset = Some(u.update_id + 1);

                            if let Some(msg) = u.message {
                                if let Some(text) = msg.text {
                                    let chat_id = msg.chat.id;
                                    let uname = msg.from.as_ref().and_then(|f| f.username.as_deref());
                                    let cmd_text = text.trim();
                                    if let Err(e) = handle_command(cmd_text, chat_id, uname, &ctx).await {
                                        error!("Ошибка обработки команды {}: {:?}", cmd_text, e);
                                    }
                                }
                            } else if let Some(cb) = u.callback_query {
                                let _ = bot.answer_callback_query(&cb.id).await;
                                if let (Some(msg), Some(data)) = (cb.message, cb.data) {
                                    let chat_id = msg.chat.id;
                                    let uname = cb.from.username.as_deref();
                                    let cmd = match data.as_str() {
                                        "btn_today" => "/today",
                                        "btn_tomorrow" => "/tomorrow",
                                        "btn_week" => "/week",
                                        "btn_link" => "/link",
                                        "btn_check" => "/check",
                                        _ => continue,
                                    };
                                    if let Err(e) = handle_command(cmd, chat_id, uname, &ctx).await {
                                        error!("Ошибка обработки callback {}: {:?}", cmd, e);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Сетевая ошибка поллинга Telegram: {:?}. Повтор через 3с...", e);
                        tokio::select! {
                            _ = shutdown.changed() => break,
                            _ = tokio::time::sleep(tokio::time::Duration::from_secs(3)) => {}
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Lesson;

    #[test]
    fn test_parse_command() {
        assert_eq!(parse_command("/start"), ("/start", ""));
        assert_eq!(parse_command("  /start  "), ("/start", ""));
        assert_eq!(parse_command("/group М14О-101БВ-26"), ("/group", "М14О-101БВ-26"));
        assert_eq!(parse_command("/group@my_bot М14О-101БВ-26"), ("/group", "М14О-101БВ-26"));
        assert_eq!(parse_command("/today@my_bot"), ("/today", ""));
        assert_eq!(parse_command("plain text"), ("plain", "text"));
    }

    #[test]
    fn test_get_webcal_links() {
        let config = Config {
            bot_token: "".to_string(),
            telegram_chat_id: None,
            default_group: "TEST".to_string(),
            host: "0.0.0.0".to_string(),
            port: 8000,
            base_url: "https://example.com/".to_string(),
            check_interval_minutes: 30,
            db_path: "data/db.sqlite".to_string(),
            timezone: "Europe/Moscow".to_string(),
            alert_minutes_before: 15,
        };

        let (webcal, https) = get_webcal_links(&config, "GROUP-1");
        assert_eq!(https, "https://example.com/calendar/GROUP-1.ics");
        assert_eq!(webcal, "webcal://example.com/calendar/GROUP-1.ics");

        let sub_url = get_subscribe_url(&config, "GROUP-1");
        assert_eq!(sub_url, "https://example.com/subscribe/GROUP-1");
    }

    #[test]
    fn test_format_day_schedule_empty() {
        let target_date = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let formatted = format_day_schedule(None, &target_date);
        assert!(formatted.contains("Пар нет, можно отдыхать!"));
    }

    #[test]
    fn test_format_day_schedule_with_lessons() {
        let target_date = NaiveDate::from_ymd_opt(2026, 9, 3).unwrap();
        let day = DaySchedule {
            date: target_date,
            day_of_week: "Чт".to_string(),
            lessons: vec![Lesson {
                subject: "Математический анализ <1>".to_string(),
                time_start: "09:00:00".to_string(),
                time_end: "10:30:00".to_string(),
                rooms: vec!["101 & 102".to_string()],
                lectors: vec!["Иванов И. И.".to_string()],
                lesson_types: vec!["ЛК".to_string()],
                lms: Some("https://lms.mai.ru?course=1&mod=2".to_string()),
                teams: None,
                other: None,
            }],
        };

        let formatted = format_day_schedule(Some(&day), &target_date);
        assert!(formatted.contains("Математический анализ &lt;1&gt;"));
        assert!(formatted.contains("101 &amp; 102"));
        assert!(formatted.contains("course=1&amp;mod=2"));
    }
}
