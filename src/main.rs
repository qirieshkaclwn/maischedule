mod api;
mod calendar;
mod config;
mod db;
mod diff;
mod models;
mod scheduler;
mod server;
mod telegram;
mod utils;

use anyhow::Result;
use std::net::SocketAddr;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::config::Config;
use crate::db::Database;
use crate::scheduler::run_scheduler;
use crate::server::{create_router, AppState};
use crate::telegram::{run_polling, TelegramBot};

#[tokio::main]
async fn main() -> Result<()> {
    // Инициализация легковесного логирования
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "maischedule=info,tower_http=info".into()),
        )
        .with(tracing_subscriber::fmt::layer().compact())
        .init();

    info!("Запуск MAI Schedule (Rust Edition)...");

    let config = Config::from_env();
    info!("Группа по умолчанию: {}", config.default_group);
    info!(
        "Календарь доступен по адресу: {}/calendar/{}.ics",
        config.base_url, config.default_group
    );

    // Канал координации корректного завершения всех задач
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

    // Инициализация SQLite
    let db = Database::new(&config.db_path)?;

    // Общий HTTP клиент с пулом соединений (таймаут 60с для поддержки long-polling Telegram)
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;

    // Настройка и запуск Axum веб-сервера с graceful shutdown
    let state = AppState {
        db: db.clone(),
        config: config.clone(),
        client: http_client.clone(),
    };

    let router = create_router(state);
    let addr: SocketAddr = format!("{}:{}", config.host, config.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("HTTP/Webcal сервер слушает http://{}", addr);

    let mut server_shutdown_rx = shutdown_rx.clone();
    let server_handle = tokio::spawn(async move {
        let server = axum::serve(listener, router).with_graceful_shutdown(async move {
            let _ = server_shutdown_rx.changed().await;
        });
        if let Err(e) = server.await {
            tracing::error!("Ошибка HTTP сервера: {:?}", e);
        }
    });

    // Настройка бота Telegram
    let bot = if !config.bot_token.trim().is_empty() {
        let b = TelegramBot::new(config.bot_token.clone(), http_client.clone());
        let b_poll = b.clone();
        let db_poll = db.clone();
        let cfg_poll = config.clone();
        let cl_poll = http_client.clone();
        let poll_shutdown = shutdown_rx.clone();

        tokio::spawn(async move {
            run_polling(b_poll, db_poll, cfg_poll, cl_poll, poll_shutdown).await;
        });

        Some(b)
    } else {
        tracing::warn!("BOT_TOKEN не указан. Бот Telegram отключен, работает только Webcal-сервер.");
        None
    };

    // Запуск фонового планировщика проверки расписания
    let sched_db = db.clone();
    let sched_cfg = config.clone();
    let sched_client = http_client.clone();
    let sched_shutdown = shutdown_rx.clone();
    let scheduler_handle = tokio::spawn(async move {
        run_scheduler(sched_db, sched_cfg, sched_client, bot, sched_shutdown).await;
    });

    // Ожидание сигнала завершения (Ctrl+C или SIGTERM в контейнере)
    wait_for_shutdown_signal().await;
    info!("Получен сигнал завершения. Остановка сервиса...");

    let _ = shutdown_tx.send(true);
    let _ = tokio::join!(server_handle, scheduler_handle);

    info!("Все сервисы успешно остановлены.");
    Ok(())
}

async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Не удалось установить обработчик Ctrl+C");
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!("Не удалось установить обработчик SIGTERM: {:?}", e);
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
