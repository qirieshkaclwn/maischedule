use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{watch, Semaphore};
use tracing::{error, info, warn};

use crate::api::{fetch_groups, fetch_schedule};
use crate::config::Config;
use crate::db::Database;
use crate::diff::{detect_diff, filter_current_and_future_changes, format_diff_message};
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

        // 2. Фоновый парсинг и обновление расписаний для всех остальных групп (не из списка активных)
        // Выполняется 2 раза в день (интервал: config.all_groups_sync_hours ч, по умолчанию 12 ч)
        let sync_interval_secs = (config.all_groups_sync_hours * 3600) as i64;
        let should_sync_all = match db.get_last_all_groups_sync().await {
            Ok(Some(last_sync)) => {
                let elapsed = chrono::Utc::now().signed_duration_since(last_sync);
                let needed = elapsed.num_seconds() >= sync_interval_secs;
                if !needed {
                    let next_in_hours = (sync_interval_secs - elapsed.num_seconds()).max(0) as f64 / 3600.0;
                    info!(
                        "Фоновый парсинг остальных групп МАИ пропущен (2 раза в день). Следующий запуск примерно через {:.1} ч.",
                        next_in_hours
                    );
                }
                needed
            }
            Ok(None) => true,
            Err(e) => {
                warn!("Ошибка проверки времени последней синхронизации групп: {:?}", e);
                true
            }
        };

        if should_sync_all {
            info!(
                "Запуск фонового обновления расписаний для остальных групп МАИ (2 раза в день, интервал {} ч)...",
                config.all_groups_sync_hours
            );
            if sync_all_group_schedules(&db, &client, &active_groups, shutdown.clone()).await {
                if let Err(e) = db.set_last_all_groups_sync(chrono::Utc::now()).await {
                    error!("Ошибка сохранения времени синхронизации групп: {:?}", e);
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
    info!("Проверка расписания для группы: {}", group);
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
                            let today = chrono::Local::now().date_naive();
                            let upcoming_diff = filter_current_and_future_changes(&diff, today);

                            if !upcoming_diff.is_empty() {
                                let msg_text = format_diff_message(group, &upcoming_diff);
                                let subs = db.get_subscribers_for_group(group).await.unwrap_or_default();
                                let mut targets: HashSet<i64> = subs.into_iter().collect();

                                // Если у группы нет явных подписчиков (например, это default_group до регистрации первого пользователя),
                                // но настроен TELEGRAM_CHAT_ID, отправляем уведомление на него как дефолтному получателю.
                                if targets.is_empty() && group == config.default_group.trim() {
                                    if let Some(chat_id) = config.telegram_chat_id {
                                        targets.insert(chat_id);
                                    }
                                }

                                for chat_id in targets {
                                    if let Err(e) = tg_bot.send_message(chat_id, &msg_text, None).await {
                                        error!("Ошибка отправки уведомления в чат {}: {:?}", chat_id, e);
                                    }
                                }
                            } else {
                                info!(
                                    "Все зафиксированные изменения группы {} относятся к прошедшим датам, уведомление не требуется.",
                                    group
                                );
                            }
                        }
                    } else {
                        // Обновляем снимок, чтобы актуализировать дату
                        let _ = db.save_snapshot(&new_sched).await;
                        info!("Расписание группы {} актуально (изменений нет).", group);
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
    mut shutdown: watch::Receiver<bool>,
) -> bool {
    let all_groups = match db.get_all_groups().await {
        Ok(g) => g,
        Err(e) => {
            error!("Ошибка получения списка групп из БД для фонового парсинга: {:?}", e);
            return false;
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
        return true;
    }

    info!("Фоновый парсинг расписаний для {} групп...", total);

    // Семафор на 6 одновременных запросов, чтобы бережно опрашивать сервер МАИ
    let semaphore = Arc::new(Semaphore::new(6));
    let mut join_set = tokio::task::JoinSet::new();

    for (idx, group) in to_check.into_iter().enumerate() {
        let current_num = idx + 1;

        if *shutdown.borrow() {
            info!("Остановка фонового парсинга расписаний по сигналу shutdown.");
            join_set.abort_all();
            return false;
        }

        let permit = tokio::select! {
            _ = shutdown.changed() => {
                info!("Остановка фонового парсинга расписаний по сигналу shutdown.");
                join_set.abort_all();
                return false;
            }
            p = semaphore.clone().acquire_owned() => {
                match p {
                    Ok(permit) => permit,
                    Err(_) => break,
                }
            }
        };

        info!("[{}/{}] Парсинг расписания группы: {}", current_num, total, group);

        let client = client.clone();
        let db = db.clone();
        let group_clone = group.clone();

        join_set.spawn(async move {
            let _permit = permit;
            match fetch_schedule(&client, &group_clone).await {
                Ok(sched) => {
                    let days_cnt = sched.days.len();
                    if let Err(e) = db.save_snapshot(&sched).await {
                        warn!("Ошибка сохранения снимка для группы {}: {:?}", group_clone, e);
                    } else {
                        info!("[{}/{}] Расписание сохранено для группы: {} (дней: {})", current_num, total, group_clone, days_cnt);
                    }
                }
                Err(e) => {
                    tracing::debug!("[{}/{}] Не удалось загрузить расписание для группы {}: {:?}", current_num, total, group_clone, e);
                }
            }
        });

        // Небольшая задержка между отправками запросов
        tokio::select! {
            _ = shutdown.changed() => {
                info!("Остановка фонового парсинга расписаний по сигналу shutdown.");
                join_set.abort_all();
                return false;
            }
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
    }

    // Дожидаемся завершения оставшихся запущенных фоновых задач парсинга
    while let Some(res) = tokio::select! {
        _ = shutdown.changed() => {
            info!("Остановка фонового парсинга расписаний по сигналу shutdown.");
            join_set.abort_all();
            return false;
        }
        next = join_set.join_next() => next,
    } {
        if let Err(e) = res {
            if !e.is_cancelled() {
                warn!("Фоновая задача парсинга завершилась с ошибкой: {:?}", e);
            }
        }
    }

    info!("Фоновый парсинг расписаний успешно завершен для всех {} групп МАИ.", total);
    true
}
