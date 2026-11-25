//! Место по IP (спецификация §8): поиск в локальной базе DB-IP, локальные сети, нет файла,
//! повторное открытие при смене файла, место в списке сеансов и пуш о новой стране.

use std::net::IpAddr;

use canterlot_hub::{owner_auth::geo::Geo, testkit::Method};
use serde_json::Value;
use sqlx::PgPool;

use crate::support::{
    auth::{AuthHub, PASSWORD, session_token},
    mmdb,
};

fn ip(value: &str) -> IpAddr {
    value.parse().unwrap()
}

async fn geo_hub(pool: PgPool, dir: &tempfile::TempDir) -> AuthHub {
    let path = dir.path().join("city.mmdb");
    mmdb::sample().write(&path);
    AuthHub::with_geo(pool, path).await
}

#[tokio::test]
async fn auth_geo_sessions_show_the_cached_location() {
    let pool = canterlot_hub::testkit::db().await;
    let dir = tempfile::tempdir().unwrap();
    let hub = geo_hub(pool, &dir).await;
    hub.set_password(PASSWORD).await;
    let signed = hub.sign_in_owner(&hub.anonymous(Some("203.0.113.9"))).await;
    assert_eq!(signed.status, 200);
    let body: Value = signed.json();
    assert_eq!(body["session"]["location"]["city"], "Москва");
    assert_eq!(body["session"]["location"]["country_code"], "RU");
    assert_eq!(body["session"]["location"]["local"], false);
    let token = session_token(&signed);
    let stored: Option<Value> =
        sqlx::query_scalar("select created_location from owner_sessions where id=$1::uuid")
            .bind(body["session"]["id"].as_str().unwrap())
            .fetch_one(&hub.pool)
            .await
            .unwrap();
    assert_eq!(stored.unwrap()["city"], "Москва", "cached on the row");
    // Новый адрес: место обновляется по последнему адресу.
    let from_berlin = hub.with_session(&token, Some("198.18.0.7"));
    let list: Value = from_berlin
        .send(Method::GET, "/api/v1/auth/sessions", None, None)
        .await
        .json();
    let current = &list["sessions"][0];
    assert_eq!(current["current"], true);
    assert_eq!(current["last_ip"], "198.18.0.7");
    assert_eq!(current["location"]["city"], "Berlin");
    // Локальный адрес.
    let local = hub.with_session(&token, Some("192.168.1.5"));
    let list: Value = local
        .send(Method::GET, "/api/v1/auth/sessions", None, None)
        .await
        .json();
    assert_eq!(list["sessions"][0]["location"]["local"], true);
    assert_eq!(list["sessions"][0]["location"]["city"], Value::Null);
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

/// Поддельный download.db-ip.com: отдаёт файлы по имени, остальное 404; запоминает запросы.
async fn fake_dbip(
    files: Vec<(&'static str, Vec<u8>)>,
) -> (std::sync::Arc<std::sync::Mutex<Vec<String>>>, String) {
    use axum::{extract::Path, http::StatusCode, routing::get};
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let files = std::sync::Arc::new(files);
    let log = seen.clone();
    let app = axum::Router::new().route(
        "/free/{name}",
        get(
            move |Path(name): Path<String>, headers: axum::http::HeaderMap| {
                let files = files.clone();
                let log = log.clone();
                async move {
                    assert!(headers.get("cookie").is_none());
                    log.lock().unwrap().push(name.clone());
                    match files.iter().find(|(file, _)| *file == name) {
                        Some((_, body)) => (StatusCode::OK, body.clone()),
                        None => (StatusCode::NOT_FOUND, Vec::new()),
                    }
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (seen, format!("http://{addr}/free"))
}

fn entries(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn date(year: i32, month: u32, day: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

#[tokio::test]
async fn auth_geo_update_downloads_the_current_month_and_renames_atomically() {
    let (seen, base) = fake_dbip(vec![(
        "dbip-city-lite-2026-11.mmdb.gz",
        gzip(&mmdb::sample().bytes()),
    )])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("geoip/dbip-city-lite.mmdb");
    let done = canterlot_hub::owner_auth::geo::update(&target, &base, date(2026, 11, 12))
        .await
        .unwrap();
    assert_eq!(done, "dbip-city-lite-2026-11");
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["dbip-city-lite-2026-11.mmdb.gz"]
    );
    assert_eq!(
        entries(&dir.path().join("geoip")),
        vec!["dbip-city-lite.mmdb"]
    );
    let geo = Geo::open(&target);
    assert_eq!(
        geo.lookup(ip("203.0.113.9")).unwrap().city.as_deref(),
        Some("Москва")
    );
}
