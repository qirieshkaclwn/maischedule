//! Сборщик расписания МАИ.
//!
//! Запускается на машине внутри сети МАИ, раз в сутки скачивает справочник групп
//! и расписание всех групп, упаковывает в tar.gz и отправляет на основной сервер.
//! Если задан BYPASS_INTERFACE, ходит в сеть через этот интерфейс, минуя VPN.

mod bypass;

use anyhow::{Context, Result};
use flate2::write::GzEncoder;
use flate2::Compression;
use md5::{Digest as _, Md5};
use sha2::Sha256;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

const RU_ROOT_CA: &[u8] = include_bytes!("../../certs/russian_trusted_root_ca.pem");
const RU_SUB_CA: &[u8] = include_bytes!("../../certs/russian_trusted_sub_ca.pem");

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

#[derive(Clone, Debug)]
struct Cfg {
    mai_base: String,
    ingest_url: String,
    ingest_token: String,
    interval: Duration,
    retry: Duration,
    concurrency: usize,
    iface: Option<String>,
    state_path: PathBuf,
}

impl Cfg {
    fn from_env() -> Self {
        let _ = dotenv_hint();
        let num = |key: &str, default: u64| {
            std::env::var(key)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };

        Self {
            mai_base: std::env::var("MAI_BASE_URL")
                .unwrap_or_else(|_| "https://public.mai.ru/schedule/data".into())
                .trim_end_matches('/')
                .to_string(),
            ingest_url: std::env::var("INGEST_URL").unwrap_or_default(),
            ingest_token: std::env::var("INGEST_TOKEN").unwrap_or_default(),
            interval: Duration::from_secs(num("INTERVAL_HOURS", 24).max(1) * 3600),
            retry: Duration::from_secs(num("RETRY_MINUTES", 15) * 60),
            concurrency: num("CONCURRENCY", 6).max(1) as usize,
            iface: std::env::var("BYPASS_INTERFACE")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            state_path: std::env::var("STATE_PATH")
                .unwrap_or_else(|_| "collector-state".into())
                .into(),
        }
    }
}

fn dotenv_hint() {
    // .env не обязателен, но если лежит рядом — подхватим
    let _ = dotenvy_lite();
}

fn dotenvy_lite() -> Option<()> {
    let text = std::fs::read_to_string(".env").ok()?;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            if std::env::var_os(k).is_none() {
                std::env::set_var(k, v.trim().trim_matches('"'));
            }
        }
    }
    Some(())
}

fn group_hash(name: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(name.trim().as_bytes());
    format!("{:x}", hasher.finalize())
}

#[derive(serde::Deserialize)]
struct GroupInfo {
    name: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mai_collector=info".into()),
        )
        .compact()
        .init();

    let cfg = Cfg::from_env();
    let once = std::env::args().any(|a| a == "--once");

    if cfg.ingest_url.is_empty() || cfg.ingest_token.is_empty() {
        anyhow::bail!("не заданы INGEST_URL и/или INGEST_TOKEN");
    }
    tracing::info!("источник: {}", cfg.mai_base);
    tracing::info!("приёмник: {}", cfg.ingest_url);

    let (clients, _proxy) = build_clients(&cfg).await?;
    preflight(&clients.mai, &cfg).await;

    loop {
        match sync_once(&cfg, &clients).await {
            Ok(true) => {
                tracing::info!("снапшот отправлен");
                if once {
                    break;
                }
                tracing::info!(
                    "следующая отправка через {} ч",
                    cfg.interval.as_secs() / 3600
                );
                tokio::time::sleep(cfg.interval).await;
            }
            Ok(false) => {
                tracing::info!("расписание не изменилось, отправка пропущена");
                if once {
                    break;
                }
                tokio::time::sleep(cfg.interval).await;
            }
            Err(e) => {
                tracing::error!("синхронизация не удалась: {:#}", e);
                if once {
                    return Err(e);
                }
                tracing::info!("повтор через {} мин", cfg.retry.as_secs() / 60);
                tokio::time::sleep(cfg.retry).await;
            }
        }
    }

    Ok(())
}

struct Clients {
    /// Запросы к public.mai.ru: при заданном BYPASS_INTERFACE идут мимо VPN.
    mai: reqwest::Client,
    /// Отправка снапшота на сервер: всегда обычным маршрутом.
    ingest: reqwest::Client,
}

fn base_builder() -> Result<reqwest::ClientBuilder> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60));

    // public.mai.ru выдаёт сертификат от УЦ Минцифры, его нет в webpki-roots
    for pem in [RU_ROOT_CA, RU_SUB_CA] {
        builder = builder.add_root_certificate(
            reqwest::Certificate::from_pem(pem)
                .context("разбор встроенного сертификата Минцифры")?,
        );
    }
    if let Ok(path) = std::env::var("CA_FILE") {
        let pem = std::fs::read(&path).with_context(|| format!("чтение {}", path))?;
        builder = builder.add_root_certificate(
            reqwest::Certificate::from_pem(&pem).with_context(|| format!("разбор {}", path))?,
        );
    }
    Ok(builder)
}

async fn build_clients(cfg: &Cfg) -> Result<(Clients, Option<bypass::ProxyHandle>)> {
    let mut builder = base_builder()?;
    let mut handle = None;

    if let Some(iface) = cfg.iface.as_deref() {
        match bypass::spawn(iface).await {
            Ok(proxy) => {
                let url = format!("http://127.0.0.1:{}", proxy.port);
                tracing::info!("обход VPN включён: интерфейс {}, прокси {}", iface, url);
                builder = builder.proxy(reqwest::Proxy::all(&url)?);
                handle = Some(proxy);
            }
            Err(e) => tracing::error!(
                "обход VPN не включён, пойду как есть (через туннель): {:#}",
                e
            ),
        }
    } else {
        tracing::warn!("BYPASS_INTERFACE не задан — запросы пойдут по обычному маршруту");
    }

    Ok((
        Clients {
            mai: builder.build()?,
            ingest: base_builder()?.build()?,
        },
        handle,
    ))
}

async fn preflight(client: &reqwest::Client, cfg: &Cfg) {
    let url = format!("{}/groups.json", cfg.mai_base);
    let started = std::time::Instant::now();
    match client.get(&url).send().await {
        Ok(resp) => tracing::info!(
            "проверка доступа: HTTP {} за {:.2} с",
            resp.status(),
            started.elapsed().as_secs_f64()
        ),
        Err(e) => tracing::error!("проверка доступа не прошла: {:#}", anyhow::Error::new(e)),
    }
}

async fn sync_once(cfg: &Cfg, clients: &Clients) -> Result<bool> {
    let groups_raw = get_bytes(&clients.mai, &format!("{}/groups.json", cfg.mai_base)).await?;
    let groups: Vec<GroupInfo> =
        serde_json::from_slice(&groups_raw).context("разбор groups.json")?;
    let total = groups.len();
    tracing::info!("групп в справочнике: {}", total);
    if total == 0 {
        anyhow::bail!("справочник групп пуст");
    }

    let sem = Arc::new(Semaphore::new(cfg.concurrency));
    let mut set = tokio::task::JoinSet::new();
    for group in groups {
        let client = clients.mai.clone();
        let base = cfg.mai_base.clone();
        let sem = sem.clone();
        set.spawn(async move {
            let _permit = sem.acquire_owned().await.ok()?;
            let hash = group_hash(&group.name);
            match get_bytes(&client, &format!("{}/{}.json", base, hash)).await {
                Ok(bytes) => Some((format!("{}.json", hash), bytes)),
                Err(e) => {
                    tracing::debug!("{}: {:#}", group.name, e);
                    None
                }
            }
        });
    }

    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    while let Some(joined) = set.join_next().await {
        if let Ok(Some((name, bytes))) = joined {
            files.insert(name, bytes);
        }
    }

    tracing::info!("получено расписаний: {} из {}", files.len(), total);
    if files.len() * 2 < total {
        anyhow::bail!(
            "получена только половина расписаний ({} из {}), отправку отменяю, чтобы не затереть данные",
            files.len(),
            total
        );
    }

    let payload = pack(&groups_raw, &files)?;
    let digest = hex(&Sha256::digest(&payload));

    if let Ok(prev) = std::fs::read_to_string(&cfg.state_path) {
        if prev.trim() == digest {
            return Ok(false);
        }
    }

    tracing::info!(
        "отправка снапшота: {} файлов, {:.1} МБ",
        files.len() + 1,
        payload.len() as f64 / 1_048_576.0
    );

    let resp = clients
        .ingest
        .post(&cfg.ingest_url)
        .header("x-ingest-token", &cfg.ingest_token)
        .header("content-type", "application/gzip")
        .body(payload)
        .send()
        .await
        .context("отправка снапшота")?;

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("сервер ответил {}: {}", status, body.trim());
    }
    tracing::info!("сервер принял снапшот: {}", body.trim());

    let _ = std::fs::write(&cfg.state_path, &digest);
    Ok(true)
}

fn pack(groups_raw: &[u8], files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut builder = tar::Builder::new(encoder);

    append(&mut builder, "groups.json", groups_raw)?;
    for (name, bytes) in files {
        append(&mut builder, name, bytes)?;
    }

    builder.finish()?;
    let encoder = builder.into_inner()?;
    Ok(encoder.finish()?)
}

fn append(builder: &mut tar::Builder<GzEncoder<Vec<u8>>>, name: &str, data: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_cksum();
    builder.append_data(&mut header, name, data)?;
    Ok(())
}

async fn get_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let mut last_err = None;
    for attempt in 1..=3u64 {
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                return Ok(resp.bytes().await?.to_vec());
            }
            Ok(resp) => {
                last_err = Some(anyhow::anyhow!("HTTP {} по {}", resp.status(), url));
            }
            Err(e) => {
                last_err = Some(anyhow::anyhow!("{}: {:#}", url, anyhow::Error::new(e)));
            }
        }
        tokio::time::sleep(Duration::from_millis(400 * attempt)).await;
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("запрос {} не удался", url)))
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push_str(&format!("{:02x}", b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_known_group() {
        assert_eq!(
            group_hash("М14О-101БВ-26"),
            "debcbad33560cdb3b93c753df8221a3d"
        );
        assert_eq!(
            group_hash(" М14О-101БВ-26 "),
            "debcbad33560cdb3b93c753df8221a3d"
        );
    }

    #[test]
    fn pack_produces_readable_archive() {
        use std::io::Read;
        let mut files = BTreeMap::new();
        files.insert("abc.json".to_string(), b"{\"a\":1}".to_vec());
        let payload = pack(b"[{\"name\":\"X\"}]", &files).unwrap();

        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&payload[..]));
        let mut names = Vec::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            names.push(entry.path().unwrap().to_string_lossy().to_string());
            let mut s = String::new();
            entry.read_to_string(&mut s).unwrap();
        }
        assert_eq!(names, vec!["groups.json", "abc.json"]);
    }
}
