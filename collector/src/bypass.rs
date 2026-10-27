//! Локальный HTTP CONNECT-прокси, который ходит в сеть мимо VPN.
//!
//! На машинах с политической маршрутизацией (Happ/xray, tun0, отдельная таблица)
//! весь трафик уходит в туннель. Сокет с SO_BINDTODEVICE привязывается к физическому
//! интерфейсу, и таблица туннеля для него не срабатывает — маршрут берётся из main.
//! Требуется CAP_NET_RAW: `sudo setcap cap_net_raw+ep ./mai-collector`.

use anyhow::{Context, Result};
use socket2::{Domain, Protocol, Socket, Type};
use std::io;
use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub struct ProxyHandle {
    pub port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for ProxyHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Поднимает прокси на 127.0.0.1 со случайным портом и привязкой к `iface`.
pub async fn spawn(iface: &str) -> Result<ProxyHandle> {
    // Проверяем право на SO_BINDTODEVICE сразу, чтобы не узнать об этом в момент запроса
    probe_device(iface)?;

    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let iface = iface.to_string();

    let task = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tracing::debug!("прокси: новое соединение");
                    let iface = iface.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle(stream, &iface).await {
                            tracing::warn!("прокси: {:#}", e);
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!("прокси: ошибка accept: {}", e);
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    });

    Ok(ProxyHandle { port, task })
}

fn probe_device(iface: &str) -> Result<()> {
    let sock = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    set_device(&sock, iface).with_context(|| {
        format!(
            "не удалось привязаться к интерфейсу {} (нужен CAP_NET_RAW: sudo setcap cap_net_raw+ep ./mai-collector)",
            iface
        )
    })
}

fn set_device(sock: &Socket, iface: &str) -> Result<()> {
    let name = std::ffi::CString::new(iface)?;
    let rc = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            name.as_ptr() as *const libc::c_void,
            (name.as_bytes().len() + 1) as libc::socklen_t,
        )
    };
    if rc != 0 {
        let err = io::Error::last_os_error();
        anyhow::bail!("SO_BINDTODEVICE({}) -> {}", iface, err);
    }
    Ok(())
}

async fn handle(mut client: TcpStream, iface: &str) -> Result<()> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(15), client.read(&mut chunk))
            .await
            .context("таймаут чтения заголовков")??;
        if n == 0 {
            anyhow::bail!("клиент закрыл соединение до CONNECT");
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > 16 * 1024 {
            anyhow::bail!("слишком длинный заголовок");
        }
    }

    let head = String::from_utf8_lossy(&buf);
    let first = head.lines().next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();

    if !method.eq_ignore_ascii_case("CONNECT") {
        let _ = client
            .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n")
            .await;
        anyhow::bail!("метод {} не поддерживается", method);
    }

    let (host, port) = split_host_port(target)?;
    let addr = tokio::net::lookup_host((host.as_str(), port))
        .await?
        .find(SocketAddr::is_ipv4)
        .with_context(|| format!("не удалось разрешить {}", target))?;

    let upstream = connect_via_device(addr, iface).await?;
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;

    let (mut cr, mut cw) = client.into_split();
    let (mut ur, mut uw) = upstream.into_split();
    let up = tokio::spawn(async move { tokio::io::copy(&mut cr, &mut uw).await });
    let down = tokio::spawn(async move { tokio::io::copy(&mut ur, &mut cw).await });
    let _ = tokio::join!(up, down);
    Ok(())
}

async fn connect_via_device(addr: SocketAddr, iface: &str) -> Result<TcpStream> {
    let sock = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP))?;
    sock.set_nonblocking(true)?;
    set_device(&sock, iface)?;

    match sock.connect(&addr.into()) {
        Ok(_) => {}
        Err(e) if e.raw_os_error() == Some(libc::EINPROGRESS) => {}
        Err(e) => return Err(e).with_context(|| format!("connect {}", addr)),
    }

    let std_stream: std::net::TcpStream = sock.into();
    let stream = TcpStream::from_std(std_stream)?;
    tokio::time::timeout(Duration::from_secs(15), stream.writable())
        .await
        .context("таймаут подключения")??;
    if let Some(err) = stream.take_error()? {
        return Err(err).with_context(|| format!("connect {}", addr));
    }
    Ok(stream)
}

fn split_host_port(target: &str) -> Result<(String, u16)> {
    let (host, port) = target
        .rsplit_once(':')
        .with_context(|| format!("некорректная цель CONNECT: {}", target))?;
    Ok((host.trim_matches(['[', ']']).to_string(), port.parse()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_host_port_works() {
        assert_eq!(
            split_host_port("public.mai.ru:443").unwrap(),
            ("public.mai.ru".to_string(), 443)
        );
        assert_eq!(
            split_host_port("[::1]:8443").unwrap(),
            ("::1".to_string(), 8443)
        );
    }
}
