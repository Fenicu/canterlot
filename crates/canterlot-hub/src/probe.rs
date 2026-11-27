//! Закрытая проверка готовности: только адрес прослушивания из конфига и один запрос к `/readyz`.

use std::{
    fs,
    io::Write,
    net::{Ipv6Addr, SocketAddr},
    path::Path,
    time::Duration,
};

use serde::Deserialize;
use serde_json::Value;

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Из конфига берём только точку прослушивания: остальные поля ведут к секретам и пробе не нужны.
#[derive(Deserialize)]
struct ProbeConfig {
    http: Option<ProbeHttp>,
}

#[derive(Deserialize)]
struct ProbeHttp {
    listen: SocketAddr,
}

/// Хаб внутри контейнера слушает все интерфейсы, а проверка идёт из того же контейнера.
fn local(listen: SocketAddr) -> SocketAddr {
    match listen {
        SocketAddr::V4(addr) if addr.ip().is_unspecified() => {
            SocketAddr::from(([127, 0, 0, 1], addr.port()))
        }
        SocketAddr::V6(addr) if addr.ip().is_unspecified() => {
            SocketAddr::from((Ipv6Addr::LOCALHOST, addr.port()))
        }
        other => other,
    }
}

struct Verdict {
    ready: bool,
    status: Value,
}

/// `Err` — конфиг непрочитаем или ответ не получен; это отдельный код выхода от «не готов».
async fn check(path: &Path) -> Result<Verdict, ()> {
    let text = fs::read_to_string(path).map_err(|_| ())?;
    let config: ProbeConfig = toml::from_str(&text).map_err(|_| ())?;
    let listen = config.http.ok_or(())?.listen;
    let url = format!("http://{}/readyz", local(listen));
    // Прокси из окружения не должен подменять обращение к локальному хабу.
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(PROBE_TIMEOUT)
        .build()
        .map_err(|_| ())?;
    let response = client.get(url).send().await.map_err(|_| ())?;
    let ready = response.status().is_success();
    let status = response.json::<Value>().await.map_err(|_| ())?;
    Ok(Verdict { ready, status })
}

/// Печатает ровно одну строку JSON и возвращает код выхода: 0 готов, 1 не готов, 2 недоступен.
pub async fn run(path: &Path) -> u8 {
    let result = check(path).await;
    let code = match &result {
        Ok(verdict) if verdict.ready => 0,
        Ok(_) => 1,
        Err(()) => 2,
    };
    let (ready, status) = match result {
        Ok(verdict) => (verdict.ready, verdict.status),
        Err(()) => (false, Value::Null),
    };
    let line = format!(
        "{{\"ready\":{ready},\"version\":\"{}\",\"status\":{}}}",
        env!("CARGO_PKG_VERSION"),
        serde_json::to_string(&status).unwrap_or_else(|_| "null".to_owned())
    );
    if writeln!(std::io::stdout().lock(), "{line}").is_err() {
        return 2;
    }
    code
}
