//! Закрытая проверка готовности: `canterlot-hub probe` читает из конфига только адрес
//! прослушивания HTTP и печатает вердикт одной строкой JSON.

use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use canterlot_hub::{
    config::HubTimers,
    testkit::{db, scratch_dir, spawn_test_hub_http, test_http_config},
};
use serde_json::{Value, json};

type BoxError = Box<dyn std::error::Error + Send + Sync>;
type TestResult = Result<(), BoxError>;

/// Дочерний процесс ждём асинхронно: в том же рантайме живёт HTTP-сервер тестового хаба.
async fn probe(config: &Path) -> Result<(i32, Value), BoxError> {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_canterlot-hub"))
        .args(["probe", "--config"])
        .arg(config)
        .output()
        .await?;
    let stdout = String::from_utf8(output.stdout)?;
    assert_eq!(stdout.lines().count(), 1, "one JSON line: {stdout:?}");
    let value: Value = serde_json::from_str(stdout.trim())?;
    Ok((output.status.code().expect("exit code"), value))
}

fn config_with_listen(dir: &Path, listen: &str) -> Result<PathBuf, BoxError> {
    let config = dir.join("hub.toml");
    fs::write(&config, format!("[http]\nlisten = '{listen}'\n"))?;
    Ok(config)
}

/// Флаги здоровья фоновых служб стартуют выключенными: ждём их включения, а не время.
async fn wait_services_up(pool: &sqlx::PgPool) -> TestResult {
    tokio::time::timeout(Duration::from_secs(15), async {
        while !canterlot_hub::singleton::down_services(pool).is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test]
async fn probe_ready_hub_exits_zero_and_prints_version() -> TestResult {
    let pool = db().await;
    let hub = spawn_test_hub_http(pool.clone(), HubTimers::default(), test_http_config()).await;
    wait_services_up(&pool).await?;
    let dir = scratch_dir()?;
    let config = config_with_listen(dir.path(), &hub.http_addr().to_string())?;
    let (code, value) = probe(&config).await?;
    assert_eq!(code, 0);
    assert_eq!(value["ready"], json!(true));
    assert_eq!(value["version"], json!(env!("CARGO_PKG_VERSION")));
    assert_eq!(value["status"], json!({"status": "ok"}));
    let _ = hub.shutdown.send(());
    let _ = tokio::time::timeout(Duration::from_secs(10), hub.join).await;
    Ok(())
}

/// Привязка к `0.0.0.0` в конфиге означает тот же порт на loopback: внутри контейнера
/// хаб слушает все адреса, а проба идёт только к локальному процессу.
#[tokio::test]
async fn probe_wildcard_listen_uses_loopback() -> TestResult {
    let pool = db().await;
    let hub = spawn_test_hub_http(pool.clone(), HubTimers::default(), test_http_config()).await;
    wait_services_up(&pool).await?;
    let dir = scratch_dir()?;
    let config = config_with_listen(dir.path(), &format!("0.0.0.0:{}", hub.http_addr().port()))?;
    let (code, value) = probe(&config).await?;
    assert_eq!(code, 0, "{value}");
    assert_eq!(value["status"], json!({"status": "ok"}));
    let _ = hub.shutdown.send(());
    let _ = tokio::time::timeout(Duration::from_secs(10), hub.join).await;
    Ok(())
}

#[tokio::test]
async fn probe_not_ready_exits_one() -> TestResult {
    let pool = db().await;
    let hub = spawn_test_hub_http(pool.clone(), HubTimers::default(), test_http_config()).await;
    wait_services_up(&pool).await?;
    // Расхождение установки с манифестом — тот же отказ, что ловит проверка готовности
    // в http_core: детерминированное состояние базы, без ожидания по времени.
    sqlx::query("update installation set restore_generation = $1")
        .bind(uuid::Uuid::now_v7())
        .execute(&pool)
        .await?;
    let dir = scratch_dir()?;
    let config = config_with_listen(dir.path(), &hub.http_addr().to_string())?;
    let (code, value) = probe(&config).await?;
    assert_eq!(code, 1, "{value}");
    assert_eq!(value["ready"], json!(false));
    assert_eq!(
        value["status"],
        json!({"status": "not_ready", "reason": "database"})
    );
    let _ = hub.shutdown.send(());
    let _ = tokio::time::timeout(Duration::from_secs(10), hub.join).await;
    Ok(())
}

#[tokio::test]
async fn probe_unreachable_exits_two() -> TestResult {
    let dir = scratch_dir()?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let config = config_with_listen(dir.path(), &format!("127.0.0.1:{port}"))?;
    let (code, value) = probe(&config).await?;
    assert_eq!(code, 2, "{value}");
    assert_eq!(value["ready"], json!(false));
    assert_eq!(value["status"], Value::Null);
    Ok(())
}

#[tokio::test]
async fn probe_bad_config_exits_two() -> TestResult {
    let dir = scratch_dir()?;
    let config = dir.path().join("hub.toml");
    fs::write(&config, "database_url_file = 'db.url'\n")?;
    let (code, value) = probe(&config).await?;
    assert_eq!(code, 2, "{value}");
    assert_eq!(value["status"], Value::Null);
    Ok(())
}
