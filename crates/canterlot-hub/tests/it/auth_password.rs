//! Вход по паролю и защита от перебора (ADR-0005, спецификация §4.1, §9).

use std::time::Duration;

use canterlot_hub::{
    owner_auth::{DEVICE_COOKIE, SESSION_COOKIE, password::Passwords, recovery},
    testkit::{Method, TEST_OWNER_LOGIN, TestResponse},
};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::support::auth::{AuthHub, PASSWORD, error_code, set_cookie};

fn normalized(response: &TestResponse) -> (u16, Value) {
    let mut body: Value = response.json();
    body["error"]["correlation_id"] = Value::Null;
    (response.status.as_u16(), body)
}

async fn failures(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from auth_failures")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn retry_after(response: &TestResponse) -> u64 {
    let body: Value = response.json();
    body["error"]["details"]["retry_after_seconds"]
        .as_u64()
        .expect("retry_after_seconds")
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_password_sign_in_sets_host_cookies_and_returns_the_session(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    let client = hub.anonymous(Some("203.0.113.9")).with_header(
        "User-Agent",
        "Mozilla/5.0 (X11; Linux x86_64; rv:159.0) Firefox/159.0",
    );
    let response = client
        .send(
            Method::POST,
            "/api/v1/auth/password",
            None,
            Some(json!({"login": TEST_OWNER_LOGIN, "password": PASSWORD, "device": {"platform": "Linux"}})),
        )
        .await;
    assert_eq!(response.status, 200, "{:?}", response.body);
    let (token, attributes) = set_cookie(&response, SESSION_COOKIE).unwrap();
    assert_eq!(token.len(), 43);
    assert_eq!(
        attributes,
        "Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=7776000"
    );
    let (device, attributes) = set_cookie(&response, DEVICE_COOKIE).unwrap();
    assert_eq!(device.len(), 43);
    assert_eq!(
        attributes,
        "Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=34560000"
    );
    let body: Value = response.json();
    let session = &body["session"];
    assert_eq!(session["method"], "password");
    assert_eq!(session["created_ip"], "203.0.113.9");
    assert_eq!(session["last_ip"], "203.0.113.9");
    assert_eq!(session["current"], true);
    assert_eq!(session["location"], Value::Null);
    assert_eq!(session["label"], "Firefox 159 · Linux");
    let stored: Option<String> = sqlx::query_scalar(
        "select phc from owner_passwords where owner_id=$1 and phc like '$argon2id$%'",
    )
    .bind(hub.owner_id())
    .fetch_optional(&hub.pool)
    .await
    .unwrap();
    assert!(stored.is_some());
    // Токен в базе только хешем.
    let plain: i64 = sqlx::query_scalar(
        "select count(*) from owner_sessions where encode(token_sha256,'escape') like '%'||$1||'%'",
    )
    .bind(&token)
    .fetch_one(&hub.pool)
    .await
    .unwrap();
    assert_eq!(plain, 0);

    let me = hub
        .with_session(&token, Some("203.0.113.9"))
        .send(Method::GET, "/api/v1/me", None, None)
        .await;
    assert_eq!(me.status, 200);
    assert_eq!(me.json::<Value>()["session_id"], session["id"]);
    // Свежий вход не продлевает cookie на каждом запросе.
    assert!(set_cookie(&me, SESSION_COOKIE).is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_password_wrong_login_wrong_password_and_no_password_look_the_same(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let client = hub.anonymous(Some("203.0.113.10"));
    let none = hub.sign_in_owner(&client).await;
    assert_eq!(error_code(&none), "invalid_credentials");
    assert_eq!(none.status, 401);
    assert!(set_cookie(&none, SESSION_COOKIE).is_none());
    hub.set_password(PASSWORD).await;
    let wrong_login = hub.sign_in(&client, "Owner", PASSWORD).await;
    let wrong_password = hub
        .sign_in(&client, TEST_OWNER_LOGIN, "wrong password!")
        .await;
    assert_eq!(normalized(&wrong_login), normalized(&none));
    assert_eq!(normalized(&wrong_password), normalized(&none));
    assert_eq!(failures(&hub.pool).await, 3);
    let ok = hub.sign_in_owner(&client).await;
    assert_eq!(ok.status, 200);
    // Успех очищает корзину адреса.
    let left: i64 =
        sqlx::query_scalar("select count(*) from auth_buckets where key='ip:203.0.113.10'")
            .fetch_one(&hub.pool)
            .await
            .unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_password_bucket_locks_after_five_failures_and_doubles(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    let attacker = hub.anonymous(Some("203.0.113.20"));
    for _ in 0..5 {
        let response = hub
            .sign_in(&attacker, TEST_OWNER_LOGIN, "guess guess guess")
            .await;
        assert_eq!(error_code(&response), "invalid_credentials");
    }
    let locked = hub.sign_in_owner(&attacker).await;
    assert_eq!(locked.status, 429);
    assert_eq!(error_code(&locked), "too_many_attempts");
    let wait = retry_after(&locked);
    assert!((1..=60).contains(&wait), "{wait}");
    assert_eq!(
        locked.headers["retry-after"].to_str().unwrap(),
        wait.to_string()
    );
    assert_eq!(
        failures(&hub.pool).await,
        5,
        "a refused attempt is not verified"
    );
    // Другой адрес не задет.
    assert_eq!(
        hub.sign_in_owner(&hub.anonymous(Some("203.0.113.21")))
            .await
            .status,
        200
    );

    let expire = "update auth_buckets set locked_until=now()-interval '1 second' where key='ip:203.0.113.20'";
    sqlx::query(expire).execute(&hub.pool).await.unwrap();
    assert_eq!(
        error_code(
            &hub.sign_in(&attacker, TEST_OWNER_LOGIN, "guess guess guess")
                .await
        ),
        "invalid_credentials"
    );
    let doubled = retry_after(&hub.sign_in_owner(&attacker).await);
    assert!((61..=120).contains(&doubled), "{doubled}");
    for _ in 0..10 {
        sqlx::query(expire).execute(&hub.pool).await.unwrap();
        hub.sign_in(&attacker, TEST_OWNER_LOGIN, "guess guess guess")
            .await;
    }
    let capped = retry_after(&hub.sign_in_owner(&attacker).await);
    assert!((3000..=3600).contains(&capped), "{capped}");

    // Пятнадцать минут без неудач и без замка сбрасывают счёт.
    sqlx::query(
        "update auth_buckets set locked_until=now()-interval '16 minutes',
           last_failure_at=now()-interval '16 minutes' where key='ip:203.0.113.20'",
    )
    .execute(&hub.pool)
    .await
    .unwrap();
    hub.sign_in(&attacker, TEST_OWNER_LOGIN, "guess guess guess")
        .await;
    let count: i32 =
        sqlx::query_scalar("select failures from auth_buckets where key='ip:203.0.113.20'")
            .fetch_one(&hub.pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    assert_eq!(hub.sign_in_owner(&attacker).await.status, 200);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_password_ipv6_addresses_share_their_64_and_mapped_ipv4_is_ipv4(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    for host in 1..=5 {
        let client = hub.anonymous(Some(&format!("2001:db8:1:2::{host:x}")));
        hub.sign_in(&client, TEST_OWNER_LOGIN, "guess guess guess")
            .await;
    }
    let same_net = hub.anonymous(Some("2001:db8:1:2:ffff:eeee:dddd:cccc"));
    assert_eq!(hub.sign_in_owner(&same_net).await.status, 429);
    let next_net = hub.anonymous(Some("2001:db8:1:3::1"));
    assert_eq!(hub.sign_in_owner(&next_net).await.status, 200);

    for _ in 0..5 {
        hub.sign_in(
            &hub.anonymous(Some("::ffff:203.0.113.7")),
            TEST_OWNER_LOGIN,
            "guess guess guess",
        )
        .await;
    }
    assert_eq!(
        hub.sign_in_owner(&hub.anonymous(Some("203.0.113.7")))
            .await
            .status,
        429
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_password_parallel_attempts_cannot_exceed_the_bucket(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    let client = hub.anonymous(Some("203.0.113.30"));
    let attempts = (0..20).map(|_| {
        let client = client.clone();
        async move {
            client
                .send(
                    Method::POST,
                    "/api/v1/auth/password",
                    None,
                    Some(json!({"login": TEST_OWNER_LOGIN, "password": "guess guess guess"})),
                )
                .await
                .status
                .as_u16()
        }
    });
    let statuses = futures_util::future::join_all(attempts).await;
    assert_eq!(
        statuses.iter().filter(|s| **s == 401).count(),
        5,
        "{statuses:?}"
    );
    assert_eq!(
        statuses.iter().filter(|s| **s == 429).count(),
        15,
        "{statuses:?}"
    );
    assert_eq!(failures(&hub.pool).await, 5);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_password_global_lock_spares_known_devices_and_other_methods(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    sqlx::query("insert into push_settings(singleton,waiting,turn_finished,failures) values(true,true,true,true)")
        .execute(&hub.pool)
        .await
        .unwrap();
    let first = hub
        .sign_in_owner(&hub.anonymous(Some("198.51.100.50")))
        .await;
    let (device, _) = set_cookie(&first, DEVICE_COOKIE).unwrap();

    for n in 0..31 {
        let client = hub.anonymous(Some(&format!("192.0.2.{}", n + 1)));
        assert_eq!(
            error_code(
                &hub.sign_in(&client, TEST_OWNER_LOGIN, "guess guess guess")
                    .await
            ),
            "invalid_credentials"
        );
    }
    let fresh_ip = hub.anonymous(Some("203.0.113.99"));
    let locked = hub.sign_in_owner(&fresh_ip).await;
    assert_eq!(locked.status, 429);
    assert!((500..=600).contains(&retry_after(&locked)));
    let locks: i64 =
        sqlx::query_scalar("select count(*) from owner_pushes where kind='sign_in_lock'")
            .fetch_one(&hub.pool)
            .await
            .unwrap();
    assert_eq!(locks, 1, "one push per lock");

    let known = fresh_ip
        .clone()
        .with_header("Cookie", &format!("{DEVICE_COOKIE}={device}"));
    let response = hub.sign_in_owner(&known).await;
    assert_eq!(response.status, 200);
    assert_eq!(
        set_cookie(&response, DEVICE_COOKIE).unwrap().0,
        device,
        "known device is kept"
    );
    // Чужая или выдуманная cookie устройства не освобождает от блокировки.
    let forged = fresh_ip.clone().with_header(
        "Cookie",
        &format!("{DEVICE_COOKIE}=canterlot-test-session-token-0123456789abcd"),
    );
    assert_eq!(hub.sign_in_owner(&forged).await.status, 429);

    // Восстановление не знает общей блокировки.
    let token = recovery::issue(&hub.pool, hub.owner_id(), 10)
        .await
        .unwrap();
    let recovered = fresh_ip
        .send(
            Method::POST,
            "/api/v1/auth/recovery",
            None,
            Some(json!({"token": token})),
        )
        .await;
    assert_eq!(recovered.status, 200);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_password_known_device_has_its_own_bucket(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    let first = hub
        .sign_in_owner(&hub.anonymous(Some("198.51.100.60")))
        .await;
    let (device, _) = set_cookie(&first, DEVICE_COOKIE).unwrap();
    let ip = "198.51.100.61";
    let known = hub
        .anonymous(Some(ip))
        .with_header("Cookie", &format!("{DEVICE_COOKIE}={device}"));
    for _ in 0..5 {
        hub.sign_in(
            &hub.anonymous(Some(ip)),
            TEST_OWNER_LOGIN,
            "guess guess guess",
        )
        .await;
    }
    assert_eq!(
        hub.sign_in_owner(&hub.anonymous(Some(ip))).await.status,
        429
    );
    assert_eq!(
        hub.sign_in_owner(&known).await.status,
        200,
        "device bucket is separate"
    );
    for _ in 0..5 {
        hub.sign_in(&known, TEST_OWNER_LOGIN, "guess guess guess")
            .await;
    }
    assert_eq!(
        hub.sign_in_owner(&known).await.status,
        429,
        "device bucket locks too"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_password_permit_timeout_answers_429_without_counting(pool: PgPool) {
    let passwords = Passwords::new(8, 1, 1, 1, Duration::from_millis(100));
    let hub = AuthHub::build(pool, |_| {}, true, Some(passwords)).await;
    hub.set_password(PASSWORD).await;
    let held = hub.state.passwords.permit().await.unwrap();
    let response = hub
        .sign_in_owner(&hub.anonymous(Some("203.0.113.40")))
        .await;
    assert_eq!(response.status, 429);
    assert_eq!(error_code(&response), "too_many_attempts");
    assert_eq!(failures(&hub.pool).await, 0);
    let buckets: i64 = sqlx::query_scalar("select count(*) from auth_buckets")
        .fetch_one(&hub.pool)
        .await
        .unwrap();
    assert_eq!(buckets, 0);
    drop(held);
    assert_eq!(
        hub.sign_in_owner(&hub.anonymous(Some("203.0.113.40")))
            .await
            .status,
        200
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_password_wrong_current_password_counts_as_failure(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    let owner = hub.owner();
    let change = |current: &str| {
        let owner = owner.clone();
        let body = json!({"current_password": current, "new_password": "a brand new password"});
        async move {
            owner
                .send(Method::PUT, "/api/v1/auth/password", None, Some(body))
                .await
        }
    };
    for _ in 0..5 {
        let response = change("not the password").await;
        assert_eq!(response.status, 403);
        assert_eq!(error_code(&response), "wrong_password");
    }
    assert_eq!(failures(&hub.pool).await, 5);
    let locked = change(PASSWORD).await;
    assert_eq!(locked.status, 429);
    assert_eq!(error_code(&locked), "too_many_attempts");
    // Сеанс при этом жив.
    let me = owner.send(Method::GET, "/api/v1/me", None, None).await;
    assert_eq!(me.status, 200);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_password_client_ip_needs_a_matched_secret_and_one_valid_header(pool: PgPool) {
    let without_secret = AuthHub::with(pool.clone(), |_| {}, false).await;
    without_secret.set_password(PASSWORD).await;
    let response = without_secret
        .sign_in_owner(&without_secret.anonymous(Some("203.0.113.9")))
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json::<Value>()["session"]["created_ip"],
        Value::Null
    );

    let hub = AuthHub::new(pool).await;
    for (name, client) in [
        ("missing", hub.anonymous(None)),
        ("garbage", hub.anonymous(Some("not-an-ip"))),
        (
            "twice",
            hub.anonymous(Some("203.0.113.9"))
                .with_header("X-Canterlot-Client-Ip", "203.0.113.10"),
        ),
    ] {
        let response = hub.sign_in_owner(&client).await;
        assert_eq!(response.status, 200, "{name}");
        assert_eq!(
            response.json::<Value>()["session"]["created_ip"],
            Value::Null,
            "{name}"
        );
    }
    let ok = hub
        .sign_in_owner(&hub.anonymous(Some(" 203.0.113.9 ")))
        .await;
    assert_eq!(ok.json::<Value>()["session"]["created_ip"], "203.0.113.9");
    // Неизвестный адрес — одна общая корзина.
    for _ in 0..5 {
        hub.sign_in(&hub.anonymous(None), TEST_OWNER_LOGIN, "guess guess guess")
            .await;
    }
    assert_eq!(
        hub.sign_in_owner(&hub.anonymous(Some("bad"))).await.status,
        429
    );
    assert_eq!(
        hub.sign_in_owner(&hub.anonymous(Some("203.0.113.9")))
            .await
            .status,
        200
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_password_endpoints_keep_csrf_and_methods_reports_a_password(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let methods = hub
        .anonymous(None)
        .send(Method::GET, "/api/v1/auth/methods", None, None)
        .await;
    assert_eq!(
        methods.json::<Value>(),
        json!({"password": false, "passkey": false})
    );
    hub.set_password(PASSWORD).await;
    let methods = hub
        .anonymous(None)
        .send(Method::GET, "/api/v1/auth/methods", None, None)
        .await;
    assert_eq!(
        methods.json::<Value>(),
        json!({"password": true, "passkey": false})
    );
    let no_client_header = hub.anonymous(None).without_header("X-Canterlot-Client");
    let response = hub.sign_in_owner(&no_client_header).await;
    assert_eq!(response.status, 403);
    assert_eq!(error_code(&response), "origin_rejected");
    // Без секрета прокси публичный путь закрыт транспортным шлюзом.
    let no_secret = hub.anonymous(None).without_header("X-Canterlot-Proxy");
    let response = hub.sign_in_owner(&no_secret).await;
    assert_eq!(error_code(&response), "unauthenticated");
}
