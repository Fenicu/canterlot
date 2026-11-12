//! Ключи доступа (спецификация §4.2): полный обряд WebAuthn через программный ключ
//! `webauthn-authenticator-rs` SoftPasskey той же версии 0.6.2, что и `webauthn-rs`.
//! Состояние обряда одноразовое и живёт 5 минут, добавление и удаление требуют свежести,
//! неудачный finish считается неудачей перебора, счётчик ключа обновляется.

use canterlot_hub::{
    owner_auth::{DEVICE_COOKIE, Method as SignInMethod, SESSION_COOKIE, passkeys},
    testkit::{Method, TEST_ORIGIN, TEST_SESSION_ID, TestClient, TestResponse},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use webauthn_authenticator_rs::{WebauthnAuthenticator, softpasskey::SoftPasskey};
use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse, Url};

use crate::support::auth::{AuthHub, PASSWORD, error_code, session_token, set_cookie};

const CHROME_ANDROID: &str = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 \
    (KHTML, like Gecko) Chrome/142.0.0.0 Mobile Safari/537.36";

fn origin() -> Url {
    Url::parse(TEST_ORIGIN).unwrap()
}

/// Программный ключ с проверкой пользователя (как отпечаток или PIN).
fn authenticator() -> SoftPasskey {
    SoftPasskey::new(true)
}

async fn post(client: &TestClient, path: &str, body: Value) -> TestResponse {
    client.send(Method::POST, path, None, Some(body)).await
}

async fn register(client: &TestClient, key: &mut SoftPasskey, name: Option<&str>) -> TestResponse {
    let start = post(client, "/api/v1/auth/passkeys/register/start", json!({})).await;
    assert_eq!(start.status, 200, "{:?}", start.body);
    let start: Value = start.json();
    let options: CreationChallengeResponse =
        serde_json::from_value(start["options"].clone()).unwrap();
    let credential = key.do_registration(origin(), options).unwrap();
    let mut body = json!({"challenge_id": start["challenge_id"], "credential": credential});
    if let Some(name) = name {
        body["name"] = json!(name);
    }
    post(client, "/api/v1/auth/passkeys/register/finish", body).await
}

/// Начало входа или повторного входа: идентификатор обряда и ответ ключа.
async fn assertion(client: &TestClient, path: &str, key: &mut SoftPasskey) -> (Value, Value) {
    let start = post(client, path, json!({})).await;
    assert_eq!(start.status, 200, "{:?}", start.body);
    let start: Value = start.json();
    let options: RequestChallengeResponse =
        serde_json::from_value(start["options"].clone()).unwrap();
    let credential = key.do_authentication(origin(), options).unwrap();
    (
        start["challenge_id"].clone(),
        serde_json::to_value(credential).unwrap(),
    )
}

async fn sign_in(hub: &AuthHub, key: &mut SoftPasskey, ip: &str) -> TestResponse {
    sign_in_with(&hub.anonymous(Some(ip)), key).await
}

async fn sign_in_with(client: &TestClient, key: &mut SoftPasskey) -> TestResponse {
    let (challenge, credential) = assertion(client, "/api/v1/auth/passkey/start", key).await;
    post(
        client,
        "/api/v1/auth/passkey/finish",
        json!({"challenge_id": challenge, "credential": credential}),
    )
    .await
}

async fn stored_counter(pool: &PgPool) -> u64 {
    let passkey: Value = sqlx::query_scalar("select passkey from owner_passkeys")
        .fetch_one(pool)
        .await
        .unwrap();
    passkey["cred"]["counter"].as_u64().unwrap()
}

async fn failures(pool: &PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from auth_failures")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_passkeys_register_list_and_sign_in_update_the_counter(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let methods: Value = hub
        .anonymous(None)
        .send(Method::GET, "/api/v1/auth/methods", None, None)
        .await
        .json();
    assert_eq!(methods["passkey"], false);
    let mut key = authenticator();
    let owner = hub.owner().with_header("User-Agent", CHROME_ANDROID);
    let added = register(&owner, &mut key, None).await;
    assert_eq!(added.status, 200, "{:?}", added.body);
    let added: Value = added.json();
    assert_eq!(
        added["name"], "Chrome 142 · Android",
        "named after the device"
    );
    assert_eq!(added["last_used_at"], Value::Null);
    let methods: Value = hub
        .anonymous(None)
        .send(Method::GET, "/api/v1/auth/methods", None, None)
        .await
        .json();
    assert_eq!(methods["passkey"], true);
    let list: Value = owner
        .send(Method::GET, "/api/v1/auth/passkeys", None, None)
        .await
        .json();
    assert_eq!(list["passkeys"].as_array().unwrap().len(), 1);
    assert_eq!(list["passkeys"][0]["id"], added["id"]);
    let before = stored_counter(&hub.pool).await;

    let signed = sign_in(&hub, &mut key, "203.0.113.70").await;
    assert_eq!(signed.status, 200, "{:?}", signed.body);
    let session = &signed.json::<Value>()["session"];
    assert_eq!(session["method"], "passkey");
    let token = session_token(&signed);
    let me = hub
        .with_session(&token, Some("203.0.113.70"))
        .send(Method::GET, "/api/v1/auth/sessions", None, None)
        .await;
    assert!(
        me.json::<Value>()["fresh_until"].is_string(),
        "passkey proves a credential"
    );
    let after = stored_counter(&hub.pool).await;
    assert!(after > before, "counter {before} -> {after}");
    let used: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("select last_used_at from owner_passkeys")
            .fetch_one(&hub.pool)
            .await
            .unwrap();
    assert!(used.is_some());
    assert_eq!(
        sign_in(&hub, &mut key, "203.0.113.70").await.status,
        200,
        "a second sign-in works with the stored counter"
    );
    assert!(stored_counter(&hub.pool).await > after);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_passkeys_challenge_is_single_use_and_expires(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let mut key = authenticator();
    assert_eq!(
        register(&hub.owner(), &mut key, Some("Ключ")).await.status,
        200
    );
    let client = hub.anonymous(Some("203.0.113.71"));
    let (challenge, credential) = assertion(&client, "/api/v1/auth/passkey/start", &mut key).await;
    let body = json!({"challenge_id": challenge, "credential": credential});
    let first = post(&client, "/api/v1/auth/passkey/finish", body.clone()).await;
    assert_eq!(first.status, 200);
    let replay = post(&client, "/api/v1/auth/passkey/finish", body).await;
    assert_eq!(replay.status, 401);
    assert_eq!(error_code(&replay), "invalid_credentials");
    assert!(set_cookie(&replay, SESSION_COOKIE).is_none());
    assert_eq!(failures(&hub.pool).await, 1, "a failed finish is a failure");

    let (challenge, credential) = assertion(&client, "/api/v1/auth/passkey/start", &mut key).await;
    sqlx::query("update owner_webauthn_challenges set expires_at=now()-interval '1 second'")
        .execute(&hub.pool)
        .await
        .unwrap();
    let late = post(
        &client,
        "/api/v1/auth/passkey/finish",
        json!({"challenge_id": challenge, "credential": credential}),
    )
    .await;
    assert_eq!(late.status, 401);
    let ttl: f64 = {
        let (_, _) = assertion(&client, "/api/v1/auth/passkey/start", &mut key).await;
        sqlx::query_scalar(
            "select extract(epoch from max(expires_at)-max(created_at))::float8
             from owner_webauthn_challenges where expires_at>now()",
        )
        .fetch_one(&hub.pool)
        .await
        .unwrap()
    };
    assert!((299.0..=301.0).contains(&ttl), "{ttl}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_passkeys_wrong_key_counts_and_locks_the_bucket(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let mut key = authenticator();
    assert_eq!(register(&hub.owner(), &mut key, None).await.status, 200);
    let client = hub.anonymous(Some("203.0.113.72"));
    for attempt in 0..5 {
        let (challenge, mut credential) =
            assertion(&client, "/api/v1/auth/passkey/start", &mut key).await;
        // Подпись испорчена: ключ тот, ответ не его.
        let signature = credential["response"]["signature"]
            .as_str()
            .unwrap()
            .to_owned();
        credential["response"]["signature"] = json!(format!("AAAA{}", &signature[4..]));
        let response = post(
            &client,
            "/api/v1/auth/passkey/finish",
            json!({"challenge_id": challenge, "credential": credential}),
        )
        .await;
        assert_eq!(response.status, 401, "attempt {attempt}");
        assert_eq!(error_code(&response), "invalid_credentials");
    }
    assert_eq!(failures(&hub.pool).await, 5);
    let (challenge, credential) = assertion(&client, "/api/v1/auth/passkey/start", &mut key).await;
    let locked = post(
        &client,
        "/api/v1/auth/passkey/finish",
        json!({"challenge_id": challenge, "credential": credential}),
    )
    .await;
    assert_eq!(locked.status, 429);
    assert_eq!(error_code(&locked), "too_many_attempts");
    // Другой адрес не заперт.
    assert_eq!(sign_in(&hub, &mut key, "198.51.100.73").await.status, 200);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_passkeys_start_is_limited_and_needs_a_key(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let client = hub.anonymous(Some("203.0.113.74"));
    let none = post(&client, "/api/v1/auth/passkey/start", json!({})).await;
    assert_eq!(none.status, 409);
    assert_eq!(error_code(&none), "no_passkeys");
    let mut key = authenticator();
    assert_eq!(register(&hub.owner(), &mut key, None).await.status, 200);
    for _ in 0..10 {
        assert_eq!(
            post(&client, "/api/v1/auth/passkey/start", json!({}))
                .await
                .status,
            200
        );
    }
    let limited = post(&client, "/api/v1/auth/passkey/start", json!({})).await;
    assert_eq!(limited.status, 429);
    assert_eq!(error_code(&limited), "too_many_attempts");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_passkeys_full_global_cap_still_lets_a_known_device_start(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    let mut key = authenticator();
    assert_eq!(register(&hub.owner(), &mut key, None).await.status, 200);
    let first = hub.sign_in_owner(&hub.anonymous(Some("198.51.100.60"))).await;
    let (device, _) = set_cookie(&first, DEVICE_COOKIE).unwrap();
    // Чужие корзины держат общий предел живых обрядов входа заполненным.
    sqlx::query(
        "insert into owner_webauthn_challenges(id,owner_id,kind,bucket,state,expires_at)
         select gen_random_uuid(),$1,'sign_in','ip:2001:db8:'||g||'::/64','{}'::jsonb,
                now()+interval '5 minutes'
         from generate_series(1,$2::int) g",
    )
    .bind(hub.owner_id())
    .bind(passkeys::MAX_SIGN_IN_CHALLENGES as i32)
    .execute(&hub.pool)
    .await
    .unwrap();
    assert_eq!(passkeys::MAX_SIGN_IN_CHALLENGES, 500);
    let stranger = hub.anonymous(Some("203.0.113.61"));
    let refused = post(&stranger, "/api/v1/auth/passkey/start", json!({})).await;
    assert_eq!(refused.status, 429);
    assert_eq!(error_code(&refused), "too_many_attempts");
    // Известное устройство: свой предел корзины, общий его не касается.
    let known = hub
        .anonymous(Some("203.0.113.61"))
        .with_header("Cookie", &format!("{DEVICE_COOKIE}={device}"));
    let response = sign_in_with(&known, &mut key).await;
    assert_eq!(response.status, 200, "{:?}", response.body);
    // Поддельная cookie устройства исключения не даёт.
    let forged = hub.anonymous(Some("203.0.113.62")).with_header(
        "Cookie",
        &format!("{DEVICE_COOKIE}=canterlot-test-session-token-0123456789abcd"),
    );
    let refused = post(&forged, "/api/v1/auth/passkey/start", json!({})).await;
    assert_eq!(refused.status, 429);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_passkeys_adding_and_deleting_need_a_fresh_session(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let mut key = authenticator();
    let owner = hub.owner();
    let added: Value = register(&owner, &mut key, Some("Телефон")).await.json();
    let id = added["id"].as_str().unwrap().to_owned();
    hub.make_stale(TEST_SESSION_ID).await;
    let start = post(&owner, "/api/v1/auth/passkeys/register/start", json!({})).await;
    assert_eq!(start.status, 403);
    assert_eq!(error_code(&start), "reauth_required");
    let delete = owner
        .send(
            Method::DELETE,
            &format!("/api/v1/auth/passkeys/{id}"),
            None,
            None,
        )
        .await;
    assert_eq!(delete.status, 403);
    assert_eq!(error_code(&delete), "reauth_required");
    // Переименование свободно.
    let renamed = owner
        .send(
            Method::PATCH,
            &format!("/api/v1/auth/passkeys/{id}"),
            None,
            Some(json!({"name": "  Bitwarden  "})),
        )
        .await;
    assert_eq!(renamed.status, 200);
    assert_eq!(renamed.json::<Value>()["name"], "Bitwarden");
    let empty = owner
        .send(
            Method::PATCH,
            &format!("/api/v1/auth/passkeys/{id}"),
            None,
            Some(json!({"name": " "})),
        )
        .await;
    assert_eq!(empty.status, 400);
    // Повторный вход ключом делает сеанс свежим, после чего удаление проходит.
    let (challenge, credential) =
        assertion(&owner, "/api/v1/auth/reauth/passkey/start", &mut key).await;
    let fresh = post(
        &owner,
        "/api/v1/auth/reauth/passkey/finish",
        json!({"challenge_id": challenge, "credential": credential}),
    )
    .await;
    assert_eq!(fresh.status, 200, "{:?}", fresh.body);
    assert!(fresh.json::<Value>()["fresh_until"].is_string());
    let delete = owner
        .send(
            Method::DELETE,
            &format!("/api/v1/auth/passkeys/{id}"),
            None,
            None,
        )
        .await;
    assert_eq!(delete.status, 200, "{:?}", delete.body);
    assert_eq!(delete.json::<Value>()["passkeys"], json!([]));
    let missing = owner
        .send(
            Method::DELETE,
            &format!("/api/v1/auth/passkeys/{id}"),
            None,
            None,
        )
        .await;
    assert_eq!(missing.status, 404);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_passkeys_reauth_makes_a_qr_session_fresh(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let mut key = authenticator();
    assert_eq!(register(&hub.owner(), &mut key, None).await.status, 200);
    let (_, token) = hub.session(SignInMethod::Qr, Some(TEST_SESSION_ID)).await;
    let qr = hub.with_session(&token, Some("203.0.113.75"));
    let start = post(&qr, "/api/v1/auth/passkeys/register/start", json!({})).await;
    assert_eq!(error_code(&start), "reauth_required");
    let (challenge, mut credential) =
        assertion(&qr, "/api/v1/auth/reauth/passkey/start", &mut key).await;
    let signature = credential["response"]["signature"]
        .as_str()
        .unwrap()
        .to_owned();
    credential["response"]["signature"] = json!(format!("AAAA{}", &signature[4..]));
    let rejected = post(
        &qr,
        "/api/v1/auth/reauth/passkey/finish",
        json!({"challenge_id": challenge, "credential": credential}),
    )
    .await;
    assert_eq!(rejected.status, 403);
    assert_eq!(error_code(&rejected), "passkey_rejected");
    assert_eq!(failures(&hub.pool).await, 1);
    let (challenge, credential) =
        assertion(&qr, "/api/v1/auth/reauth/passkey/start", &mut key).await;
    let fresh = post(
        &qr,
        "/api/v1/auth/reauth/passkey/finish",
        json!({"challenge_id": challenge, "credential": credential}),
    )
    .await;
    assert_eq!(fresh.status, 200, "{:?}", fresh.body);
    let start = post(&qr, "/api/v1/auth/passkeys/register/start", json!({})).await;
    assert_eq!(start.status, 200, "fresh after the passkey reauth");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_passkeys_challenges_are_bound_to_their_kind_and_session(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let mut key = authenticator();
    let owner = hub.owner();
    // Регистрацию, начатую одним сеансом, не завершить из другого.
    let start: Value = post(&owner, "/api/v1/auth/passkeys/register/start", json!({}))
        .await
        .json();
    let options: CreationChallengeResponse =
        serde_json::from_value(start["options"].clone()).unwrap();
    let credential = key.do_registration(origin(), options).unwrap();
    let (_, other_token) = hub.session(SignInMethod::Password, None).await;
    let other = hub.with_session(&other_token, None);
    let foreign = post(
        &other,
        "/api/v1/auth/passkeys/register/finish",
        json!({"challenge_id": start["challenge_id"], "credential": credential}),
    )
    .await;
    assert_eq!(foreign.status, 403);
    assert_eq!(error_code(&foreign), "passkey_rejected");
    assert_eq!(register(&owner, &mut key, None).await.status, 200);
    // Состояние входа не годится для повторного входа и наоборот.
    let (challenge, credential) =
        assertion(&hub.anonymous(None), "/api/v1/auth/passkey/start", &mut key).await;
    let crossed = post(
        &owner,
        "/api/v1/auth/reauth/passkey/finish",
        json!({"challenge_id": challenge, "credential": credential}),
    )
    .await;
    assert_eq!(error_code(&crossed), "passkey_rejected");
    let (challenge, credential) =
        assertion(&owner, "/api/v1/auth/reauth/passkey/start", &mut key).await;
    let crossed = post(
        &hub.anonymous(None),
        "/api/v1/auth/passkey/finish",
        json!({"challenge_id": challenge, "credential": credential}),
    )
    .await;
    assert_eq!(crossed.status, 401);
    let rows: i64 = sqlx::query_scalar("select count(*) from owner_webauthn_challenges")
        .fetch_one(&hub.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0, "every finish consumes its row, right or wrong");
}
