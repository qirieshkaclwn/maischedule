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

    pub async fn set_my_commands(&self) -> Result<()> {
        let body = json!({
            "commands": [
                { "command": "start", "description": "Главное меню и статус подключения" },
                { "command": "group", "description": "Выбрать или сменить учебную группу" },
                { "command": "today", "description": "Расписание занятий на сегодня" },
                { "command": "tomorrow", "description": "Расписание занятий на завтра" },
                { "command": "week", "description": "Расписание на текущую неделю" },
                { "command": "link", "description": "Ссылки для календаря iOS и Webcal" },
                { "command": "notifications", "description": "Включить/выключить уведомления" },
                { "command": "changes", "description": "История последних изменений" },
                { "command": "check", "description": "Проверить обновления прямо сейчас" },
                { "command": "help", "description": "Справка по всем командам" }
            ]
        });

        let resp = self
            .client
            .post(self.api_url("setMyCommands"))
            .json(&body)
            .send()
            .await
            .context("Ошибка вызова setMyCommands")?;

        let res: TelegramResponse<bool> = resp.json().await?;
        if !res.ok {
            warn!("Telegram API error in setMyCommands: {:?}", res.description);
        } else {
            info!("Список команд бота успешно зарегистрирован в Telegram.");
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

    pub async fn send_document(
        &self,
        chat_id: i64,
        filename: &str,
        data: Vec<u8>,
        caption: Option<&str>,
    ) -> Result<()> {
        let part = reqwest::multipart::Part::bytes(data)
            .file_name(filename.to_string())
            .mime_str("text/plain; charset=utf-8")?;

        let mut form = reqwest::multipart::Form::new()
            .text("chat_id", chat_id.to_string())
            .part("document", part);

        if let Some(cap) = caption {
            form = form.text("caption", cap.to_string());
        }

        let resp = self
            .client
            .post(self.api_url("sendDocument"))
            .multipart(form)
            .send()
            .await
            .context("Ошибка отправки sendDocument в Telegram")?;

        let res: TelegramResponse<serde_json::Value> = resp.json().await?;
        if !res.ok {
            warn!("Telegram API error in sendDocument: {:?}", res.description);
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

pub fn is_admin(chat_id: i64, config: &Config) -> bool {
    config.telegram_chat_id == Some(chat_id)
}

pub fn main_keyboard(
    subscribe_url: &str,
    notifications_enabled: bool,
    is_admin: bool,
) -> serde_json::Value {
    let notif_text = if notifications_enabled {
        "Уведомления: Вкл"
    } else {
        "Уведомления: Выкл"
    };

    let mut inline_keyboard = vec![
        vec![
            json!({ "text": "Сегодня", "callback_data": "btn_today" }),
            json!({ "text": "Завтра", "callback_data": "btn_tomorrow" }),
        ],
        vec![
            json!({ "text": "Расписание на неделю", "callback_data": "btn_week" }),
        ],
        vec![
            json!({ "text": "Добавить в Календарь iPhone", "url": subscribe_url }),
        ],
        vec![
            json!({ "text": "Сменить группу", "callback_data": "btn_change_group" }),
            json!({ "text": notif_text, "callback_data": "btn_toggle_notif" }),
        ],
        vec![
            json!({ "text": "Инструкция для iOS", "callback_data": "btn_link" }),
            json!({ "text": "Проверить обновления", "callback_data": "btn_check" }),
        ],
    ];

    if is_admin {
        inline_keyboard.push(vec![
            json!({ "text": "Лог обновлений (файл)", "callback_data": "btn_admin_log" }),
        ]);
    }

    json!({ "inline_keyboard": inline_keyboard })
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
            "<b>{}. {} - {}</b> [{}]\n   <b>{}</b>\n   Ауд: {}\n   Преподаватель: {}\n",
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

pub async fn handle_incoming_text(
    text: &str,
    chat_id: i64,
    username: Option<&str>,
    first_name: Option<&str>,
    ctx: &BotContext<'_>,
) -> Result<()> {
    let clean_text = crate::utils::strip_emojis(text);
    let trimmed = clean_text.trim();

    if trimmed.is_empty() {
        return Ok(());
    }

    if trimmed.starts_with('/') {
        let (cmd, args) = parse_command(trimmed);
        match cmd {
            "/start" => handle_start(chat_id, username, first_name, ctx).await,
            "/help" => handle_help(chat_id, ctx).await,
            "/link" => handle_link(chat_id, ctx).await,
            "/today" => handle_today(chat_id, ctx).await,
            "/tomorrow" => handle_tomorrow(chat_id, ctx).await,
            "/week" => handle_week(chat_id, ctx).await,
            "/group" => handle_group_command(chat_id, args, username, first_name, ctx).await,
            "/notifications" => handle_toggle_notifications(chat_id, ctx).await,
            "/changes" => handle_changes(chat_id, ctx).await,
            "/check" => handle_check(chat_id, ctx).await,
            "/admin_log" | "/adminlog" | "/log" => handle_admin_log(chat_id, ctx).await,
            _ => {
                let msg = "Неизвестная команда. Введите /help, чтобы посмотреть список доступных команд.";
                ctx.bot.send_message(chat_id, msg, None).await
            }
        }
    } else {
        // Обычное текстовое сообщение: пользователь пытается ввести или найти группу
        handle_group_search_or_set(chat_id, trimmed, username, first_name, ctx).await
    }
}

async fn handle_start(
    chat_id: i64,
    username: Option<&str>,
    first_name: Option<&str>,
    ctx: &BotContext<'_>,
) -> Result<()> {
    ctx.db.upsert_user(chat_id, None, username, first_name).await?;
    let user_opt = ctx.db.get_user(chat_id).await?;
    let is_adm = is_admin(chat_id, ctx.config);

    if let Some(user) = user_opt.filter(|u| u.group_name.is_some()) {
        let user_group = user.group_name.as_deref().unwrap();
        let subscribe_url = get_subscribe_url(ctx.config, user_group);
        let text = format!(
            "Привет, {}!\n\n\
            Сервис автосинхронизации расписания МАИ с календарем iOS / macOS / Google и уведомлений об изменениях пар.\n\n\
            Ваша учебная группа: <b>{}</b>\n\n\
            <b>Как подключить календарь на iPhone:</b>\n\
            1. Нажмите на кнопку <b>«Добавить в Календарь iPhone»</b> ниже.\n\
            2. В появившемся окне Apple Calendar нажмите <b>«Подписаться»</b>.\n\
            3. В настройках календаря включите <i>«Автообновление: Каждые 15 минут»</i>.\n\n\
            Бот уведомит вас, если пару перенесут, отменят или изменится аудитория.",
            escape_html(first_name.unwrap_or("студент")),
            escape_html(user_group)
        );
        ctx.bot
            .send_message(chat_id, &text, Some(main_keyboard(&subscribe_url, user.notifications_enabled, is_adm)))
            .await
    } else {
        let text = format!(
            "Привет, {}!\n\n\
            Я бот на Rust для автосинхронизации расписания МАИ с календарем Apple/Google и мгновенных уведомлений об изменениях пар.\n\n\
            <b>Чтобы начать, укажите вашу учебную группу:</b>\n\
            Отправьте название группы ответным сообщением (например, <code>М14О-101БВ-26</code> или просто номер, например <code>101БВ</code>).",
            escape_html(first_name.unwrap_or("студент"))
        );
        ctx.bot.send_message(chat_id, &text, None).await
    }
}

async fn handle_help(chat_id: i64, ctx: &BotContext<'_>) -> Result<()> {
    let mut text = "<b>Доступные команды:</b>\n\n\
        /start - Главное меню и статус подключения\n\
        /group &lt;название&gt; - Выбрать или сменить учебную группу\n\
        /today - Расписание на сегодня\n\
        /tomorrow - Расписание на завтра\n\
        /week - Расписание на текущую учебную неделю (Пн-Сб)\n\
        /link - Ссылки Webcal и HTTPS для календаря\n\
        /notifications - Включить или выключить уведомления об изменениях\n\
        /changes - История последних изменений в расписании\n\
        /check - Ручная проверка изменений прямо сейчас\n\
        /help - Справка по командам\n\n\
        Вы также можете просто отправить номер или название группы сообщением в чат.".to_string();

    if is_admin(chat_id, ctx.config) {
        text.push_str("\n\n<b>Команды администратора:</b>\n/admin_log - Выгрузить файл с логом изменений по всем группам");
    }

    ctx.bot.send_message(chat_id, &text, None).await
}


async fn handle_group_command(
    chat_id: i64,
    arg: &str,
    username: Option<&str>,
    first_name: Option<&str>,
    ctx: &BotContext<'_>,
) -> Result<()> {
    if arg.is_empty() {
        let current_grp = ctx.db.get_user_group(chat_id).await?;
        let grp_info = match current_grp {
            Some(g) => format!("Ваша текущая группа: <b>{}</b>\n\n", escape_html(&g)),
            None => "Группа еще не выбрана.\n\n".to_string(),
        };

        let text = format!(
            "{}Чтобы выбрать или изменить группу, отправьте команду:\n\
            <code>/group Название-Группы</code>\n\
            или просто напишите название в чат (например: <code>М14О-101БВ-26</code>).",
            grp_info
        );
        ctx.bot.send_message(chat_id, &text, None).await
    } else {
        handle_group_search_or_set(chat_id, arg, username, first_name, ctx).await
    }
}

async fn handle_group_search_or_set(
    chat_id: i64,
    query: &str,
    username: Option<&str>,
    first_name: Option<&str>,
    ctx: &BotContext<'_>,
) -> Result<()> {
    let clean_query = query.trim();

    // 1. Проверяем точное совпадение (без учета регистра) в справочнике групп
    if let Ok(Some(official_name)) = ctx.db.find_group_exact_or_ci(clean_query).await {
        return apply_user_group(chat_id, &official_name, username, first_name, ctx).await;
    }

    // 2. Поиск по подстроке среди групп МАИ
    let matches = ctx.db.search_groups(clean_query, 6).await.unwrap_or_default();

    if matches.len() == 1 {
        // Ровно одно совпадение - применяем автоматически
        let group_name = matches[0].name.clone();
        return apply_user_group(chat_id, &group_name, username, first_name, ctx).await;
    } else if matches.len() > 1 {
        // Несколько совпадений - предлагаем интерактивный выбор кнопками
        let mut buttons = Vec::new();
        for g in matches {
            buttons.push(vec![json!({
                "text": format!("{} (курс {})", g.name, g.course),
                "callback_data": format!("set_grp:{}", g.name)
            })]);
        }

        let keyboard = json!({ "inline_keyboard": buttons });
        let text = format!(
            "По запросу «{}» найдено несколько групп. Пожалуйста, выберите вашу:",
            escape_html(clean_query)
        );
        return ctx.bot.send_message(chat_id, &text, Some(keyboard)).await;
    }

    // 3. Если в локальной базе нет, пробуем напрямую запросить расписание по API МАИ
    match fetch_schedule(ctx.client, clean_query).await {
        Ok(sched) => {
            let _ = ctx.db.save_snapshot(&sched).await;
            apply_user_group(chat_id, &sched.group, username, first_name, ctx).await
        }
        Err(_) => {
            let text = format!(
                "Группа «{}» не найдена в расписании МАИ.\n\n\
                Проверьте написание или попробуйте ввести номер без института (например: <code>101БВ</code> или <code>309</code>).",
                escape_html(clean_query)
            );
            ctx.bot.send_message(chat_id, &text, None).await
        }
    }
}

async fn apply_user_group(
    chat_id: i64,
    group_name: &str,
    username: Option<&str>,
    first_name: Option<&str>,
    ctx: &BotContext<'_>,
) -> Result<()> {
    ctx.db.upsert_user(chat_id, Some(group_name), username, first_name).await?;
    ctx.db.set_user_group(chat_id, group_name).await?;

    // Предзагрузка расписания, если его еще нет в базе
    let _ = get_or_load_schedule(group_name, ctx.db, ctx.client).await;

    let user = ctx.db.get_user(chat_id).await?.unwrap_or(crate::models::User {
        id: chat_id,
        group_name: Some(group_name.to_string()),
        username: username.map(ToString::to_string),
        first_name: first_name.map(ToString::to_string),
        notifications_enabled: true,
        created_at: String::new(),
        updated_at: String::new(),
    });

    let subscribe_url = get_subscribe_url(ctx.config, group_name);
    let text = format!(
        "Группа успешно установлена: <b>{}</b>!\n\n\
        Теперь вы можете добавить расписание в Apple Calendar / Google Calendar по кнопке ниже:",
        escape_html(group_name)
    );
    let is_adm = is_admin(chat_id, ctx.config);
    ctx.bot
        .send_message(chat_id, &text, Some(main_keyboard(&subscribe_url, user.notifications_enabled, is_adm)))
        .await
}


async fn require_user_group(chat_id: i64, ctx: &BotContext<'_>) -> Result<Option<String>> {
    let grp = ctx.db.get_user_group(chat_id).await?;
    if grp.is_none() {
        let text = "Сначала укажите вашу учебную группу.\n\
            Отправьте название группы в чат (например: <code>М14О-101БВ-26</code>).";
        ctx.bot.send_message(chat_id, text, None).await?;
        Ok(None)
    } else {
        Ok(grp)
    }
}

async fn handle_link(chat_id: i64, ctx: &BotContext<'_>) -> Result<()> {
    let Some(user_group) = require_user_group(chat_id, ctx).await? else {
        return Ok(());
    };

    let (webcal_url, https_url) = get_webcal_links(ctx.config, &user_group);
    let subscribe_url = get_subscribe_url(ctx.config, &user_group);
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
        escape_html(&user_group),
        escape_html(&subscribe_url),
        escape_html(&webcal_url),
        escape_html(&https_url)
    );
    ctx.bot.send_message(chat_id, &text, None).await
}

async fn handle_today(chat_id: i64, ctx: &BotContext<'_>) -> Result<()> {
    let Some(user_group) = require_user_group(chat_id, ctx).await? else {
        return Ok(());
    };

    let today = Local::now().date_naive();
    match get_or_load_schedule(&user_group, ctx.db, ctx.client).await {
        Ok(sched) => {
            let day = sched.get_day(&today);
            let text = format!(
                "Группа: <b>{}</b>\n\n{}",
                escape_html(&user_group),
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

async fn handle_tomorrow(chat_id: i64, ctx: &BotContext<'_>) -> Result<()> {
    let Some(user_group) = require_user_group(chat_id, ctx).await? else {
        return Ok(());
    };

    let tomorrow = Local::now().date_naive() + chrono::Duration::days(1);
    match get_or_load_schedule(&user_group, ctx.db, ctx.client).await {
        Ok(sched) => {
            let day = sched.get_day(&tomorrow);
            let text = format!(
                "Группа: <b>{}</b>\n\n{}",
                escape_html(&user_group),
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

async fn handle_week(chat_id: i64, ctx: &BotContext<'_>) -> Result<()> {
    let Some(user_group) = require_user_group(chat_id, ctx).await? else {
        return Ok(());
    };

    let today = Local::now().date_naive();
    let weekday_num = today.weekday().num_days_from_monday();
    let monday = today - chrono::Duration::days(weekday_num as i64);

    match get_or_load_schedule(&user_group, ctx.db, ctx.client).await {
        Ok(sched) => {
            let mut blocks = vec![format!(
                "<b>Расписание на неделю ({}):</b>\n",
                escape_html(&user_group)
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

async fn handle_toggle_notifications(chat_id: i64, ctx: &BotContext<'_>) -> Result<()> {
    let is_enabled = ctx.db.toggle_user_notifications(chat_id).await?;
    let status_text = if is_enabled {
        "включены. Вы будете получать сообщения при отмене, переносе занятий или смене аудиторий."
    } else {
        "отключены."
    };

    let user_group = ctx.db.get_user_group(chat_id).await?;
    let is_adm = is_admin(chat_id, ctx.config);
    let reply_markup = user_group.as_deref().map(|grp| {
        let subscribe_url = get_subscribe_url(ctx.config, grp);
        main_keyboard(&subscribe_url, is_enabled, is_adm)
    });


    let text = format!("Уведомления об изменениях расписания <b>{}</b>", status_text);
    ctx.bot.send_message(chat_id, &text, reply_markup).await
}

async fn handle_changes(chat_id: i64, ctx: &BotContext<'_>) -> Result<()> {
    let Some(user_group) = require_user_group(chat_id, ctx).await? else {
        return Ok(());
    };

    match ctx.db.get_recent_changes(&user_group, 10).await {
        Ok(hist) if hist.is_empty() => {
            ctx.bot
                .send_message(
                    chat_id,
                    &format!("Для группы <b>{}</b> изменений не зафиксировано.", escape_html(&user_group)),
                    None,
                )
                .await
        }
        Ok(hist) => {
            let escaped_hist: Vec<String> = hist.into_iter().map(|h| escape_html(&h)).collect();
            let text = format!(
                "<b>Последние изменения в расписании ({}):</b>\n\n{}",
                escape_html(&user_group),
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

async fn handle_check(chat_id: i64, ctx: &BotContext<'_>) -> Result<()> {
    let Some(user_group) = require_user_group(chat_id, ctx).await? else {
        return Ok(());
    };

    match fetch_schedule(ctx.client, &user_group).await {
        Ok(new_sched) => {
            let old_sched = ctx.db.get_snapshot(&user_group).await?;
            if let Some(old) = old_sched {
                let diff = detect_diff(&old, &new_sched);
                if diff.is_empty() {
                    ctx.bot
                        .send_message(
                            chat_id,
                            &format!("Расписание группы <b>{}</b> проверено - изменений нет.", escape_html(&user_group)),
                            None,
                        )
                        .await
                } else {
                    for c in &diff {
                        let _ = ctx
                            .db
                            .log_change(&user_group, &format!("{:?}", c.change_type), &c.details)
                            .await;
                    }
                    let _ = ctx.db.save_snapshot(&new_sched).await;
                    let msg = format_diff_message(&user_group, &diff);
                    ctx.bot.send_message(chat_id, &msg, None).await
                }
            } else {
                let _ = ctx.db.save_snapshot(&new_sched).await;
                ctx.bot
                    .send_message(
                        chat_id,
                        &format!("Расписание группы <b>{}</b> сохранено. Изменений пока нет.", escape_html(&user_group)),
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

async fn handle_admin_log(chat_id: i64, ctx: &BotContext<'_>) -> Result<()> {
    if !is_admin(chat_id, ctx.config) {
        let msg = "У вас нет прав администратора для получения системного лога.";
        return ctx.bot.send_message(chat_id, msg, None).await;
    }

    let changes = ctx.db.get_all_changes(1000).await?;
    let content = crate::db::generate_changes_log_content(&changes);
    let bytes = content.into_bytes();

    let now_str = chrono::Local::now().format("%Y%m%d_%H%M%S").to_string();
    let filename = format!("changes_log_{}.txt", now_str);
    let caption = format!("Лог изменений расписания (всего записей: {})", changes.len());

    ctx.bot
        .send_document(chat_id, &filename, bytes, Some(&caption))
        .await
}

pub async fn run_polling(
    bot: TelegramBot,
    db: Database,
    config: Config,
    client: reqwest::Client,
    mut shutdown: watch::Receiver<bool>,
) {
    info!("Запуск поллинга Telegram-бота на Rust...");
    if let Err(e) = bot.set_my_commands().await {
        warn!("Не удалось автоматически зарегистрировать команды в Telegram API: {:?}", e);
    }
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
                                    let fname = msg.from.as_ref().map(|f| f.first_name.as_str());
                                    let cmd_text = text.trim();
                                    if let Err(e) = handle_incoming_text(cmd_text, chat_id, uname, fname, &ctx).await {
                                        error!("Ошибка обработки сообщения {}: {:?}", cmd_text, e);
                                    }
                                }
                            } else if let Some(cb) = u.callback_query {
                                let _ = bot.answer_callback_query(&cb.id).await;
                                let chat_id = match cb.message {
                                    Some(ref m) => m.chat.id,
                                    None => cb.from.id,
                                };
                                let uname = cb.from.username.as_deref();
                                let fname = Some(cb.from.first_name.as_str());

                                if let Some(ref data) = cb.data {
                                    let res = if let Some(grp) = data.strip_prefix("set_grp:") {
                                        apply_user_group(chat_id, grp, uname, fname, &ctx).await
                                    } else {
                                        match data.as_str() {
                                            "btn_today" => handle_today(chat_id, &ctx).await,
                                            "btn_tomorrow" => handle_tomorrow(chat_id, &ctx).await,
                                            "btn_week" => handle_week(chat_id, &ctx).await,
                                            "btn_link" => handle_link(chat_id, &ctx).await,
                                            "btn_check" => handle_check(chat_id, &ctx).await,
                                            "btn_toggle_notif" => handle_toggle_notifications(chat_id, &ctx).await,
                                            "btn_admin_log" => handle_admin_log(chat_id, &ctx).await,
                                            "btn_change_group" => {
                                                let text = "Чтобы сменить группу, отправьте её название в чат (например: <code>М14О-101БВ-26</code>).";
                                                bot.send_message(chat_id, text, None).await
                                            }
                                            _ => Ok(()),
                                        }
                                    };

                                    if let Err(e) = res {
                                        error!("Ошибка обработки callback {}: {:?}", data, e);
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
            all_groups_sync_hours: 12,
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

    #[test]
    fn test_main_keyboard_admin() {
        let kb_user = main_keyboard("https://example.com/sub", true, false);
        let s_user = serde_json::to_string(&kb_user).unwrap();
        assert!(!s_user.contains("btn_admin_log"));

        let kb_admin = main_keyboard("https://example.com/sub", true, true);
        let s_admin = serde_json::to_string(&kb_admin).unwrap();
        assert!(s_admin.contains("btn_admin_log"));
        assert!(s_admin.contains("Лог обновлений (файл)"));
    }
}

