//! Место по IP (спецификация §8): поиск в локальной базе DB-IP, локальные сети, нет файла,
//! повторное открытие при смене файла, место в списке сеансов и пуш о новой стране.

use std::{net::IpAddr, time::Duration};

use canterlot_hub::{
    owner_auth::geo::{Geo, Location},
    testkit::Method,
};
use serde_json::Value;
use sqlx::PgPool;

use crate::support::{
    auth::{AuthHub, PASSWORD, session_token},
    mmdb,
};

fn ip(value: &str) -> IpAddr {
    value.parse().unwrap()
}

#[test]
fn auth_geo_lookup_prefers_russian_names_then_english() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("city.mmdb");
    mmdb::sample().write(&path);
    let geo = Geo::open(&path);
    let moscow = geo.lookup(ip("203.0.113.9")).unwrap();
    assert_eq!(
        moscow,
        Location {
            city: Some("Москва".into()),
            region: Some("Moscow".into()),
            country: Some("Россия".into()),
            country_code: Some("RU".into()),
            local: false,
        }
    );
    assert_eq!(moscow.place(), "Москва");
    assert_eq!(geo.lookup(ip("::ffff:203.0.113.200")), Some(moscow));
    let berlin = geo.lookup(ip("198.18.0.1")).unwrap();
    assert_eq!(berlin.city.as_deref(), Some("Berlin"));
    assert_eq!(berlin.country_code.as_deref(), Some("DE"));
    let amsterdam = geo.lookup(ip("2001:db8:aaaa:1::5")).unwrap();
    assert_eq!(amsterdam.country.as_deref(), Some("Netherlands"));
    assert_eq!(geo.lookup(ip("192.0.2.1")), None, "address not in the base");
}

#[test]
fn auth_geo_local_networks_need_no_base() {
    let geo = Geo::open("/nonexistent/canterlot/geoip.mmdb");
    for local in [
        "10.1.2.3",
        "172.16.0.1",
        "192.168.1.10",
        "127.0.0.1",
        "169.254.10.1",
        "100.64.0.1",
        "100.127.255.254",
        "::1",
        "fe80::1",
        "fd00::7",
        "::ffff:192.168.0.1",
    ] {
        let found = geo.lookup(ip(local)).unwrap_or_else(|| panic!("{local}"));
        assert!(found.local, "{local}");
        assert_eq!(found.place(), "локальная сеть");
    }
    assert_eq!(geo.lookup(ip("203.0.113.9")), None, "no base, no location");
    assert_eq!(geo.lookup(ip("100.128.0.1")), None, "outside CGNAT");
}

#[test]
fn auth_geo_reopens_the_base_when_the_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("city.mmdb");
    let geo = Geo::with_check_interval(&path, Duration::ZERO);
    assert_eq!(geo.lookup(ip("203.0.113.9")), None);
    mmdb::sample().write(&path);
    assert_eq!(
        geo.lookup(ip("203.0.113.9")).unwrap().city.as_deref(),
        Some("Москва")
    );
    // Обновление кладёт новый файл переименованием, как `geoip update`.
    let next = dir.path().join("next.mmdb");
    mmdb::Mmdb::default()
        .insert(
            "203.0.113.0/24",
            &mmdb::city(&[("en", "Kazan")], &[], &[("en", "Russia")], "RU"),
        )
        .write(&next);
    let later = std::time::SystemTime::now() + Duration::from_secs(5);
    std::fs::File::options()
        .write(true)
        .open(&next)
        .unwrap()
        .set_modified(later)
        .unwrap();
    std::fs::rename(&next, &path).unwrap();
    assert_eq!(
        geo.lookup(ip("203.0.113.9")).unwrap().city.as_deref(),
        Some("Kazan")
    );
    // Без интервала проверки по умолчанию база не перечитывается на каждом запросе.
    let slow = Geo::open(&path);
    assert!(slow.lookup(ip("203.0.113.9")).is_some());
    std::fs::remove_file(&path).unwrap();
    assert!(slow.lookup(ip("203.0.113.9")).is_some(), "still mapped");
}

async fn geo_hub(pool: PgPool, dir: &tempfile::TempDir) -> AuthHub {
    let path = dir.path().join("city.mmdb");
    mmdb::sample().write(&path);
    AuthHub::with_geo(pool, path).await
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_geo_sessions_show_the_cached_location(pool: PgPool) {
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

async fn pushes(pool: &PgPool, kind: &str) -> Vec<Value> {
    sqlx::query_scalar("select payload from owner_pushes where kind=$1 order by created_at,id")
        .bind(kind)
        .fetch_all(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_geo_new_country_is_pushed_once_per_session_and_country(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let hub = geo_hub(pool, &dir).await;
    crate::auth_pushes::enable(&hub.pool).await;
    hub.set_password(PASSWORD).await;
    let signed = hub.sign_in_owner(&hub.anonymous(Some("203.0.113.9"))).await;
    let token = session_token(&signed);
    let sign_in = pushes(&hub.pool, "sign_in").await;
    assert_eq!(sign_in.len(), 1);
    let body = sign_in[0]["body"].as_str().unwrap();
    assert!(body.contains("Москва · 203.0.113.9"), "{body}");
    let id = signed.json::<Value>()["session"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    sqlx::query("update owner_sessions set name='Ноутбук' where id=$1::uuid")
        .bind(&id)
        .execute(&hub.pool)
        .await
        .unwrap();
    let get = |address: &'static str| {
        let client = hub.with_session(&token, Some(address));
        async move {
            let response = client
                .send(Method::GET, "/api/v1/auth/sessions", None, None)
                .await;
            assert_eq!(response.status, 200);
        }
    };
    get("203.0.113.10").await;
    assert!(
        pushes(&hub.pool, "session_country").await.is_empty(),
        "same country"
    );
    get("198.18.0.7").await;
    let country = pushes(&hub.pool, "session_country").await;
    assert_eq!(country.len(), 1);
    assert_eq!(
        country[0]["body"],
        "Сеанс «Ноутбук» используется из новой страны: Germany (Berlin, 198.18.0.7)"
    );
    // Обратно домой и снова в Германию: о каждой стране не больше одного раза.
    get("203.0.113.11").await;
    get("198.18.0.8").await;
    get("192.168.0.4").await;
    get("2001:db8:aaaa::9").await;
    let country = pushes(&hub.pool, "session_country").await;
    assert_eq!(country.len(), 2, "{country:?}");
    assert!(
        country[1]["body"].as_str().unwrap().contains("Netherlands"),
        "{country:?}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_geo_missing_base_gives_no_location(pool: PgPool) {
    let dir = tempfile::tempdir().unwrap();
    let hub = AuthHub::with_geo(pool, dir.path().join("absent.mmdb")).await;
    hub.set_password(PASSWORD).await;
    let signed = hub.sign_in_owner(&hub.anonymous(Some("203.0.113.9"))).await;
    assert_eq!(signed.status, 200);
    assert_eq!(signed.json::<Value>()["session"]["location"], Value::Null);
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

#[tokio::test]
async fn auth_geo_update_falls_back_to_the_previous_month() {
    let (seen, base) = fake_dbip(vec![
        (
            "dbip-city-lite-2026-12.mmdb.gz",
            gzip(&mmdb::sample().bytes()),
        ),
        (
            "dbip-city-lite-2026-10.mmdb.gz",
            gzip(&mmdb::sample().bytes()),
        ),
    ])
    .await;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("city.mmdb");
    let update = canterlot_hub::owner_auth::geo::update;
    assert_eq!(
        update(&target, &base, date(2027, 1, 1)).await.unwrap(),
        "dbip-city-lite-2026-12"
    );
    assert_eq!(
        update(&target, &base, date(2026, 11, 1)).await.unwrap(),
        "dbip-city-lite-2026-10"
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            "dbip-city-lite-2027-01.mmdb.gz",
            "dbip-city-lite-2026-12.mmdb.gz",
            "dbip-city-lite-2026-11.mmdb.gz",
            "dbip-city-lite-2026-10.mmdb.gz",
        ]
    );
    assert!(
        update(&target, &base, date(2026, 9, 5)).await.is_err(),
        "neither month is out"
    );
}

#[tokio::test]
async fn auth_geo_update_keeps_the_old_file_when_the_download_is_broken() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("city.mmdb");
    mmdb::sample().write(&target);
    let before = std::fs::read(&target).unwrap();
    for broken in [
        gzip(b"this is not a MaxMind database at all"),
        b"not even gzip".to_vec(),
        gzip(&mmdb::sample().bytes()[..40]),
    ] {
        let (_, base) = fake_dbip(vec![("dbip-city-lite-2026-11.mmdb.gz", broken)]).await;
        let result =
            canterlot_hub::owner_auth::geo::update(&target, &base, date(2026, 11, 12)).await;
        assert!(result.is_err(), "{result:?}");
        assert_eq!(std::fs::read(&target).unwrap(), before, "old file kept");
        assert_eq!(entries(dir.path()), vec!["city.mmdb"], "no temp files left");
    }
}

#[test]
fn auth_geo_update_cli_needs_no_database() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("hub.toml");
    // Каталог базы недоступен для записи: команда падает до любого сетевого запроса.
    std::fs::write(
        &config,
        "database_url_file = 'absent'\ngrpc_listen = '127.0.0.1:7443'\ntls_cert = 'c'\n\
         tls_key = 'k'\nmanifest = 'm'\n[auth.geoip]\npath = '/proc/canterlot-geoip/city.mmdb'\n",
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_canterlot-hub"))
        .args(["geoip", "update", "--config"])
        .arg(&config)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "geoip_dir_error"
    );
}
