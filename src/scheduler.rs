use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Semaphore};
use tracing::{error, info, warn};

use crate::api::{fetch_groups, fetch_schedule};
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

    // Первичная синхронизация каталога всех групп МАИ при старте
    sync_group_catalog(&db, &client).await;

    // Пауза 5 секунд перед началом первого цикла проверок (прерываемая сигналом остановки)
    tokio::select! {
        _ = shutdown.changed() => {
            info!("Остановка планировщика до первого запуска...");
            return;
        }
        _ = tokio::time::sleep(Duration::from_secs(5)) => {}
    }

    let mut last_catalog_sync = std::time::Instant::now();

    loop {
        // Раз в 24 часа обновляем справочник групп МАИ
        if last_catalog_sync.elapsed() >= Duration::from_secs(24 * 3600) {
            sync_group_catalog(&db, &client).await;
            last_catalog_sync = std::time::Instant::now();
        }

        // 1. Приоритетная проверка групп активных пользователей
        let mut active_groups = match db.get_all_active_groups().await {
            Ok(g) => g,
            Err(e) => {
                error!("Ошибка получения списка групп пользователей: {:?}", e);
                Vec::new()
            }
        };

        let default_grp = config.default_group.trim();
        if !default_grp.is_empty() && !active_groups.iter().any(|g| g == default_grp) {
            active_groups.push(default_grp.to_string());
        }

        info!("Проверка расписания для активных групп пользователей ({:?})...", active_groups.len());

        for group in &active_groups {
            if *shutdown.borrow() {
                info!("Остановка планировщика во время обхода активных групп...");
                return;
            }

            check_single_group(group, &db, &client, bot.as_ref(), &config).await;
        }

        // 2. Фоновый парсинг и обновление расписаний для всех остальных групп института
        info!("Запуск фонового обновления расписаний для всех групп МАИ...");
        sync_all_group_schedules(&db, &client, &active_groups, &shutdown).await;

        tokio::select! {
            _ = shutdown.changed() => {
                info!("Остановка планировщика...");
                break;
            }
            _ = tokio::time::sleep(interval) => {}
        }
    }
}

pub async fn sync_group_catalog(db: &Database, client: &reqwest::Client) {
    info!("Запрос списка всех групп МАИ из API...");
    match fetch_groups(client).await {
        Ok(groups) => {
            let count = groups.len();
            if let Err(e) = db.save_groups(&groups).await {
                error!("Ошибка сохранения групп в базу данных: {:?}", e);
            } else {
                info!("Справочник групп МАИ успешно обновлен: {} групп в базе.", count);
            }
        }
        Err(e) => {
            warn!("Не удалось загрузить список групп МАИ: {:?}", e);
        }
    }
}

async fn check_single_group(
    group: &str,
    db: &Database,
    client: &reqwest::Client,
    bot: Option<&TelegramBot>,
    config: &Config,
) {
    match fetch_schedule(client, group).await {
        Ok(new_sched) => {
            match db.get_snapshot(group).await {
                Ok(Some(old_sched)) => {
                    let diff = detect_diff(&old_sched, &new_sched);
                    if !diff.is_empty() {
                        info!("Найдено {} изменений в расписании группы {}!", diff.len(), group);

                        for c in &diff {
                            let _ = db.log_change(group, &format!("{:?}", c.change_type), &c.details).await;
                        }

                        let _ = db.save_snapshot(&new_sched).await;

                        if let Some(tg_bot) = bot {
                            let msg_text = format_diff_message(group, &diff);
                            let subs = db.get_subscribers_for_group(group).await.unwrap_or_default();
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
                    } else {
                        // Обновляем снимок, чтобы актуализировать дату
                        let _ = db.save_snapshot(&new_sched).await;
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
            tracing::debug!("Не удалось загрузить расписание для группы {}: {:?}", group, e);
        }
    }
}

async fn sync_all_group_schedules(
    db: &Database,
    client: &reqwest::Client,
    already_checked: &[String],
    shutdown: &watch::Receiver<bool>,
) {
    let all_groups = match db.get_all_groups().await {
        Ok(g) => g,
        Err(e) => {
            error!("Ошибка получения списка групп из БД для фонового парсинга: {:?}", e);
            return;
        }
    };

    let checked_set: HashSet<&str> = already_checked.iter().map(|s| s.as_str()).collect();
    let to_check: Vec<String> = all_groups
        .into_iter()
        .map(|g| g.name)
        .filter(|name| !checked_set.contains(name.as_str()))
        .collect();

    let total = to_check.len();
    if total == 0 {
        return;
    }

    info!("Фоновый парсинг расписаний для {} групп...", total);

    // Семафор на 6 одновременных запросов, чтобы бережно опрашивать сервер МАИ
    let semaphore = Arc::new(Semaphore::new(6));
    let mut updated_count = 0usize;

    for group in to_check {
        if *shutdown.borrow() {
            info!("Остановка фонового парсинга расписаний по сигналу shutdown.");
            return;
        }

        let permit = match semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => break,
        };

        let client = client.clone();
        let db = db.clone();
        let group_clone = group.clone();

        tokio::spawn(async move {
            let _permit = permit;
            if let Ok(sched) = fetch_schedule(&client, &group_clone).await {
                let _ = db.save_snapshot(&sched).await;
            }
        });

        updated_count += 1;
        // Небольшая задержка между отправками запросов
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    info!("Запущен фоновый сбор расписаний для {} групп МАИ.", updated_count);
}
