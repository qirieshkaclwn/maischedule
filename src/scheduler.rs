use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::api::fetch_schedule;
use crate::config::Config;
use crate::db::Database;
use crate::diff::{detect_diff, format_diff_message};
use crate::telegram::TelegramBot;

pub async fn run_scheduler(
    db: Database,
    config: Config,
    client: reqwest::Client,
    bot: Option<TelegramBot>,
    mut shutdown: watch::Receiver<bool>,
) {
    let interval = Duration::from_secs(config.check_interval_minutes.max(1) * 60);
    info!(
        "Запуск планировщика проверки расписания на Rust. Интервал: {} мин.",
        config.check_interval_minutes
    );

    // Пауза 5 секунд перед первым запуском (прерываемая сигналом остановки)
    tokio::select! {
        _ = shutdown.changed() => {
            info!("Остановка планировщика до первого запуска...");
            return;
        }
        _ = tokio::time::sleep(Duration::from_secs(5)) => {}
    }

    loop {
        let mut groups = match db.get_all_monitored_groups().await {
            Ok(g) => g,
            Err(e) => {
                error!("Ошибка получения списка отслеживаемых групп: {:?}", e);
                Vec::new()
            }
        };

        let default_grp = config.default_group.trim();
        if !default_grp.is_empty() && !groups.iter().any(|g| g == default_grp) {
            groups.push(default_grp.to_string());
        }

        info!("Периодическая проверка расписания для групп: {:?}", groups);

        for group in groups {
            if *shutdown.borrow() {
                info!("Остановка планировщика во время обхода групп...");
                return;
            }

            match fetch_schedule(&client, &group).await {
                Ok(new_sched) => {
                    match db.get_snapshot(&group).await {
                        Ok(Some(old_sched)) => {
                            let diff = detect_diff(&old_sched, &new_sched);
                            if !diff.is_empty() {
                                info!("Найдено {} изменений в расписании группы {}!", diff.len(), group);

                                for c in &diff {
                                    let _ = db.log_change(&group, &format!("{:?}", c.change_type), &c.details).await;
                                }

                                let _ = db.save_snapshot(&new_sched).await;

                                if let Some(ref tg_bot) = bot {
                                    let msg_text = format_diff_message(&group, &diff);
                                    let subs = db.get_subscribers_for_group(&group).await.unwrap_or_default();
                                    let mut targets: HashSet<i64> = subs.into_iter().collect();

                                    if let Some(chat_id) = config.telegram_chat_id {
                                        targets.insert(chat_id);
                                    }

                                    for chat_id in targets {
                                        if let Err(e) = tg_bot.send_message(chat_id, &msg_text, None).await {
                                            error!("Ошибка отправки уведомления в чат {}: {:?}", chat_id, e);
                                        }
                                    }
                                }
                            }
                        }
                        Ok(None) => {
                            info!("Первичный снимок для группы {} сохранен.", group);
                            let _ = db.save_snapshot(&new_sched).await;
                        }
                        Err(e) => {
                            error!("Ошибка чтения снимка из БД: {:?}", e);
                        }
                    }
                }
                Err(e) => {
                    warn!("Не удалось загрузить расписание для группы {}: {:?}", group, e);
                }
            }
        }

        tokio::select! {
            _ = shutdown.changed() => {
                info!("Остановка планировщика...");
                break;
            }
            _ = tokio::time::sleep(interval) => {}
        }
    }
}
