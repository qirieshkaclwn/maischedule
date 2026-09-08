use anyhow::{Context, Result};
use chrono::{Datelike, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{error, info, warn};

use crate::api::fetch_schedule;
use crate::config::Config;
use crate::db::Database;
use crate::diff::{detect_diff, format_diff_message};
use crate::models::{DaySchedule, GroupSchedule};

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

    pub async fn send_message(&self, chat_id: i64, text: &str, reply_markup: Option<serde_json::Value>) -> Result<()> {
        let mut body = json!({
            "chat_id": chat_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });

        if let Some(markup) = reply_markup {
            body.as_object_mut().unwrap().insert("reply_markup".to_string(), markup);
        }

        let resp = self
            .client
            .post(&self.api_url("sendMessage"))
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

    pub async fn edit_message_text(&self, chat_id: i64, message_id: i64, text: &str) -> Result<()> {
        let body = json!({
            "chat_id": chat_id,
            "message_id": message_id,
            "text": text,
            "parse_mode": "HTML",
            "disable_web_page_preview": true,
        });

        let resp = self
            .client
            .post(&self.api_url("editMessageText"))
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
        let _ = self.client.post(&self.api_url("answerCallbackQuery")).json(&body).send().await;
        Ok(())
    }

    pub async fn get_updates(&self, offset: Option<i64>, timeout: u32) -> Result<Vec<TelegramUpdate>> {
        let mut body = json!({
            "timeout": timeout,
            "allowed_updates": ["message", "callback_query"],
        });

        if let Some(off) = offset {
            body.as_object_mut().unwrap().insert("offset".to_string(), json!(off));
        }

        let resp = self
            .client
            .post(&self.api_url("getUpdates"))
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
    let https = format!("{}/calendar/{}.ics", base, group);
    let webcal = https.replace("http://", "webcal://").replace("https://", "webcal://");
    (webcal, https)
}

pub fn main_keyboard(webcal_url: &str) -> serde_json::Value {
    json!({
        "inline_keyboard": [
            [
                { "text": "📅 Сегодня", "callback_data": "btn_today" },
                { "text": "📆 Завтра", "callback_data": "btn_tomorrow" }
            ],
            [
                { "text": "🗓 Расписание на неделю", "callback_data": "btn_week" }
            ],
            [
                { "text": "📲 Добавить в Календарь iPhone", "url": webcal_url }
            ],
            [
                { "text": "ℹ️ Инструкция для iOS", "callback_data": "btn_link" },
                { "text": "🔔 Проверить обновления", "callback_data": "btn_check" }
            ]
        ]
    })
}

pub fn format_day_schedule(day: Option<&DaySchedule>, target_date: &NaiveDate) -> String {
    let date_str = target_date.format("%d.%m.%Y").to_string();
    let day_name = day.map(|d| d.day_of_week.as_str()).unwrap_or("");

    let day = match day {
        Some(d) if !d.lessons.is_empty() => d,
        _ => return format!("📅 <b>{} ({})</b>\n\n🎉 Пар нет, можно отдыхать!", date_str, day_name),
    };

    let mut out = format!("📅 <b>{} ({})</b>\n\n", date_str, day_name);
    for (idx, lesson) in day.lessons.iter().enumerate() {
        out.push_str(&format!(
            "<b>{}. {} – {}</b> [{}]\n   📖 <b>{}</b>\n   🏢 Ауд: {}\n   👨‍🏫 {}\n",
            idx + 1,
            lesson.time_start_clean(),
            lesson.time_end_clean(),
            lesson.type_str(),
            lesson.subject,
            lesson.room_str(),
            lesson.lector_str()
        ));
        if let Some(lms) = &lesson.lms {
            out.push_str(&format!("   🔗 LMS: {}\n", lms));
        }
        if let Some(teams) = &lesson.teams {
            out.push_str(&format!("   🔗 Teams: {}\n", teams));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

async fn get_or_load_schedule(group: &str, db: &Database, client: &reqwest::Client) -> Result<GroupSchedule> {
    if let Some(sched) = db.get_snapshot(group).await? {
        return Ok(sched);
    }
    let sched = fetch_schedule(client, group).await?;
    let _ = db.save_snapshot(&sched).await;
    Ok(sched)
}

pub async fn handle_command(
    cmd: &str,
    chat_id: i64,
    username: Option<&str>,
    bot: &TelegramBot,
    db: &Database,
    config: &Config,
    client: &reqwest::Client,
) -> Result<()> {
    let user_group = db
        .get_subscriber_group(chat_id)
        .await?
        .unwrap_or_else(|| config.default_group.clone());

    let (webcal_url, https_url) = get_webcal_links(config, &user_group);

    if cmd == "/start" {
        db.add_subscriber(chat_id, &user_group, username).await?;
        let text = format!(
            "👋 Привет!\n\n\
            Я бот на <b>Rust</b> для автосинхронизации расписания МАИ с твоим iPhone и уведомлений об изменениях.\n\n\
            👥 Твоя группа: <b>{}</b>\n\n\
            📲 <b>Как подключить календарь на iPhone:</b>\n\
            1. Нажми на кнопку <b>«Добавить в Календарь iPhone»</b> ниже.\n\
            2. iOS предложит подписаться на календарь — нажми <b>«Подписаться»</b>.\n\
            3. Включи <i>«Автообновление: Каждые 15 минут»</i>.\n\n\
            🔔 <i>Я пришлю сообщение, если пару перенесут, отменят или изменят аудиторию!</i>",
            user_group
        );
        bot.send_message(chat_id, &text, Some(main_keyboard(&webcal_url))).await?;
    } else if cmd == "/link" {
        let text = format!(
            "🔗 <b>Ссылки для синхронизации группы {}:</b>\n\n\
            📱 <b>Прямая ссылка для iOS (нажми на iPhone):</b>\n\
            <code>{}</code>\n\n\
            🌐 <b>HTTPS-ссылка для ручного добавления:</b>\n\
            <code>{}</code>\n\n\
            <b>Инструкция для ручного добавления на iPhone:</b>\n\
            1. Настройки ➡️ Календарь ➡️ Учетные записи\n\
            2. Добавить учетную запись ➡️ Другое ➡️ Подписной календарь\n\
            3. Вставьте HTTPS-ссылку выше и сохраните.",
            user_group, webcal_url, https_url
        );
        bot.send_message(chat_id, &text, None).await?;
    } else if cmd == "/today" {
        let today = Local::now().date_naive();
        match get_or_load_schedule(&user_group, db, client).await {
            Ok(sched) => {
                let day = sched.get_day(&today);
                let text = format!("👥 Группа: <b>{}</b>\n\n{}", user_group, format_day_schedule(day, &today));
                bot.send_message(chat_id, &text, None).await?;
            }
            Err(e) => {
                bot.send_message(chat_id, &format!("❌ Ошибка получения расписания: {}", e), None).await?;
            }
        }
    } else if cmd == "/tomorrow" {
        let tomorrow = Local::now().date_naive() + chrono::Duration::days(1);
        match get_or_load_schedule(&user_group, db, client).await {
            Ok(sched) => {
                let day = sched.get_day(&tomorrow);
                let text = format!("👥 Группа: <b>{}</b>\n\n{}", user_group, format_day_schedule(day, &tomorrow));
                bot.send_message(chat_id, &text, None).await?;
            }
            Err(e) => {
                bot.send_message(chat_id, &format!("❌ Ошибка получения расписания: {}", e), None).await?;
            }
        }
    } else if cmd == "/week" {
        let today = Local::now().date_naive();
        let weekday_num = today.weekday().num_days_from_monday();
        let monday = today - chrono::Duration::days(weekday_num as i64);

        match get_or_load_schedule(&user_group, db, client).await {
            Ok(sched) => {
                let mut blocks = vec![format!("🗓 <b>Расписание на неделю ({}):</b>\n", user_group)];
                for i in 0..6 {
                    let d = monday + chrono::Duration::days(i);
                    let day_sched = sched.get_day(&d);
                    if let Some(ds) = day_sched {
                        if !ds.lessons.is_empty() {
                            blocks.push(format_day_schedule(Some(ds), &d));
                            blocks.push("----------------------------".to_string());
                        }
                    }
                }
                if blocks.len() == 1 {
                    blocks.push("Занятий на этой неделе не найдено.".to_string());
                }
                bot.send_message(chat_id, &blocks.join("\n\n"), None).await?;
            }
            Err(e) => {
                bot.send_message(chat_id, &format!("❌ Ошибка: {}", e), None).await?;
            }
        }
    } else if cmd.starts_with("/group") {
        let parts: Vec<&str> = cmd.split_whitespace().collect();
        if parts.len() < 2 {
            let text = format!(
                "Текущая группа: <b>{}</b>\n\n\
                Чтобы изменить группу, отправьте команду:\n\
                <code>/group Название-Группы</code>\n\
                Например: <code>/group М14О-101БВ-26</code>",
                user_group
            );
            bot.send_message(chat_id, &text, None).await?;
        } else {
            let new_grp = parts[1].trim();
            match fetch_schedule(client, new_grp).await {
                Ok(sched) => {
                    db.save_snapshot(&sched).await?;
                    db.add_subscriber(chat_id, new_grp, username).await?;
                    let (new_webcal, _) = get_webcal_links(config, new_grp);
                    let text = format!(
                        "✅ Группа успешно обновлена на <b>{}</b>!\n\n\
                        Не забудьте обновить ссылку в календаре на iPhone:",
                        new_grp
                    );
                    bot.send_message(chat_id, &text, Some(main_keyboard(&new_webcal))).await?;
                }
                Err(_) => {
                    bot.send_message(
                        chat_id,
                        &format!("❌ Не удалось найти расписание для «{}». Проверьте написание.", new_grp),
                        None,
                    )
                    .await?;
                }
            }
        }
    } else if cmd == "/changes" {
        match db.get_recent_changes(&user_group, 10).await {
            Ok(hist) if hist.is_empty() => {
                bot.send_message(chat_id, &format!("ℹ️ Для группы <b>{}</b> изменений не зафиксировано.", user_group), None).await?;
            }
            Ok(hist) => {
                let text = format!("📜 <b>Последние изменения в расписании ({}):</b>\n\n{}", user_group, hist.join("\n"));
                bot.send_message(chat_id, &text, None).await?;
            }
            Err(e) => {
                bot.send_message(chat_id, &format!("❌ Ошибка чтения истории: {}", e), None).await?;
            }
        }
    } else if cmd == "/check" {
        match fetch_schedule(client, &user_group).await {
            Ok(new_sched) => {
                let old_sched = db.get_snapshot(&user_group).await?;
                if let Some(old) = old_sched {
                    let diff = detect_diff(&old, &new_sched);
                    if diff.is_empty() {
                        bot.send_message(chat_id, &format!("✅ Расписание группы <b>{}</b> проверено — изменений нет.", user_group), None).await?;
                    } else {
                        for c in &diff {
                            let _ = db.log_change(&user_group, &format!("{:?}", c.change_type), &c.details).await;
                        }
                        let _ = db.save_snapshot(&new_sched).await;
                        let msg = format_diff_message(&user_group, &diff);
                        bot.send_message(chat_id, &msg, None).await?;
                    }
                } else {
                    let _ = db.save_snapshot(&new_sched).await;
                    bot.send_message(chat_id, &format!("✅ Расписание группы <b>{}</b> сохранено. Изменений пока нет.", user_group), None).await?;
                }
            }
            Err(e) => {
                bot.send_message(chat_id, &format!("❌ Ошибка при проверке: {}", e), None).await?;
            }
        }
    }

    Ok(())
}

pub async fn run_polling(bot: TelegramBot, db: Database, config: Config, client: reqwest::Client) {
    info!("Запуск поллинга Telegram-бота на Rust...");
    let mut offset = None;

    loop {
        match bot.get_updates(offset, 25).await {
            Ok(updates) => {
                for u in updates {
                    offset = Some(u.update_id + 1);

                    if let Some(msg) = u.message {
                        if let Some(text) = msg.text {
                            let chat_id = msg.chat.id;
                            let uname = msg.from.as_ref().and_then(|f| f.username.as_deref());
                            let cmd = text.trim();
                            if let Err(e) = handle_command(cmd, chat_id, uname, &bot, &db, &config, &client).await {
                                error!("Ошибка обработки команды {}: {:?}", cmd, e);
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
                            if let Err(e) = handle_command(cmd, chat_id, uname, &bot, &db, &config, &client).await {
                                error!("Ошибка обработки callback {}: {:?}", cmd, e);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                warn!("Сетевая ошибка поллинга Telegram: {:?}. Повтор через 3с...", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
            }
        }
    }
}
