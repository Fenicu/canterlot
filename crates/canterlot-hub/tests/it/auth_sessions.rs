//! Сеансы: бездействие, отзыв с причиной, надгробия, каскад по QR, предел 50,
//! редкая запись `last_seen_at`, список и настройки (спецификация §5).

use canterlot_hub::{
    owner_auth::{Method as SignInMethod, SESSION_COOKIE, sessions},
    testkit::{Method, TEST_SESSION_ID, TestResponse},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::support::auth::{AuthHub, PASSWORD, error_code, session_token, set_cookie};

async fn me(hub: &AuthHub, token: &str) -> TestResponse {
    hub.with_session(token, Some("198.51.100.7"))
        .send(Method::GET, "/api/v1/me", None, None)
        .await
}

fn reason(response: &TestResponse) -> Value {
    response.json::<Value>()["error"]["details"]["reason"].clone()
}

fn cleared(response: &TestResponse) -> bool {
    set_cookie(response, SESSION_COOKIE)
        .is_some_and(|(value, attributes)| value.is_empty() && attributes.ends_with("Max-Age=0"))
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_sessions_idle_session_expires_and_follows_the_setting(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (id, token) = hub.session(SignInMethod::Password, None).await;
    sqlx::query("update owner_sessions set last_seen_at=now()-interval '91 days' where id=$1")
        .bind(id)
        .execute(&hub.pool)
        .await
        .unwrap();
    let response = me(&hub, &token).await;
    assert_eq!(response.status, 401);
    assert_eq!(error_code(&response), "session_expired");
    assert!(cleared(&response));
    assert_eq!(hub.revoke_reason(id).await.as_deref(), Some("idle"));
    assert_eq!(error_code(&me(&hub, &token).await), "session_expired");

    let (id, token) = hub.session(SignInMethod::Password, None).await;
    sqlx::query("update owner_sessions set last_seen_at=now()-interval '8 days' where id=$1")
        .bind(id)
        .execute(&hub.pool)
        .await
        .unwrap();
    assert_eq!(
        me(&hub, &token).await.status,
        200,
        "within the default 90 days"
    );
    sqlx::query("update owner_sessions set last_seen_at=now()-interval '8 days' where id=$1")
        .bind(id)
        .execute(&hub.pool)
        .await
        .unwrap();
    let settings = hub
        .owner()
        .send(
            Method::PATCH,
            "/api/v1/auth/settings",
            None,
            Some(json!({"idle_timeout_days": 7})),
        )
        .await;
    assert_eq!(settings.status, 200);
    assert_eq!(settings.json::<Value>(), json!({"idle_timeout_days": 7}));
    let (_, attributes) = set_cookie(&settings, SESSION_COOKIE).unwrap();
    assert!(attributes.ends_with("Max-Age=604800"), "{attributes}");
    assert_eq!(error_code(&me(&hub, &token).await), "session_expired");
    for bad in [
        json!({"idle_timeout_days": 8}),
        json!({"idle_timeout_days": 0}),
        json!({}),
    ] {
        let response = hub
            .owner()
            .send(Method::PATCH, "/api/v1/auth/settings", None, Some(bad))
            .await;
        assert_eq!(response.status, 400);
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_sessions_revoked_session_names_the_reason_and_survives_as_a_tombstone(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (other, token) = hub.session(SignInMethod::Password, None).await;
    let deleted = hub
        .owner()
        .send(
            Method::DELETE,
            &format!("/api/v1/auth/sessions/{other}"),
            None,
            None,
        )
        .await;
    assert_eq!(deleted.status, 200);
    assert_eq!(deleted.json::<Value>()["revoked"], json!([other]));
    assert!(!cleared(&deleted), "the caller keeps its cookie");
    let response = me(&hub, &token).await;
    assert_eq!(response.status, 401);
    assert_eq!(error_code(&response), "session_revoked");
    assert_eq!(reason(&response), "owner");
    assert!(cleared(&response));

    // Через 30 дней полная строка уходит, надгробие остаётся.
    sqlx::query("update owner_sessions set revoked_at=now()-interval '31 days' where id=$1")
        .bind(other)
        .execute(&hub.pool)
        .await
        .unwrap();
    sessions::purge(&hub.pool).await.unwrap();
    let rows: i64 = sqlx::query_scalar("select count(*) from owner_sessions where id=$1")
        .bind(other)
        .fetch_one(&hub.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    let response = me(&hub, &token).await;
    assert_eq!(error_code(&response), "session_revoked");
    assert_eq!(reason(&response), "owner");
    // Через 400 дней пропадает и надгробие.
    sqlx::query("update owner_session_tombstones set revoked_at=now()-interval '401 days'")
        .execute(&hub.pool)
        .await
        .unwrap();
    sessions::purge(&hub.pool).await.unwrap();
    assert_eq!(error_code(&me(&hub, &token).await), "unauthenticated");

    let unknown = hub
        .owner()
        .send(
            Method::DELETE,
            &format!("/api/v1/auth/sessions/{}", Uuid::now_v7()),
            None,
            None,
        )
        .await;
    assert_eq!(unknown.status, 404);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_sessions_logout_revokes_only_itself_and_clears_the_cookie(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (phone, phone_token) = hub.session(SignInMethod::Password, None).await;
    let (approved, approved_token) = hub.session(SignInMethod::Qr, Some(phone)).await;
    let response = hub
        .with_session(&phone_token, None)
        .send(Method::POST, "/api/v1/auth/logout", None, None)
        .await;
    assert_eq!(response.status, 200);
    assert_eq!(response.json::<Value>()["revoked"], json!([phone]));
    assert!(cleared(&response));
    assert!(
        response
            .headers
            .get_all("set-cookie")
            .iter()
            .all(|value| !value
                .to_str()
                .unwrap()
                .starts_with("__Host-canterlot_device")),
        "the device cookie stays"
    );
    let again = me(&hub, &phone_token).await;
    assert_eq!(reason(&again), "logout");
    assert_eq!(
        hub.revoke_reason(approved).await,
        None,
        "logout does not cascade"
    );
    assert_eq!(me(&hub, &approved_token).await.status, 200);
    // DELETE своего id — тот же выход и без свежести.
    hub.make_stale(approved).await;
    let own = hub
        .with_session(&approved_token, None)
        .send(
            Method::DELETE,
            &format!("/api/v1/auth/sessions/{approved}"),
            None,
            None,
        )
        .await;
    assert_eq!(own.status, 200);
    assert!(cleared(&own));
    assert_eq!(hub.revoke_reason(approved).await.as_deref(), Some("logout"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_sessions_owner_revocation_cascades_over_qr_approvals(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (phone, _) = hub.session(SignInMethod::Password, None).await;
    let (laptop, laptop_token) = hub.session(SignInMethod::Qr, Some(phone)).await;
    let (tablet, _) = hub.session(SignInMethod::Qr, Some(laptop)).await;
    let (unrelated, _) = hub.session(SignInMethod::Password, None).await;

    let list: Value = hub
        .owner()
        .send(Method::GET, "/api/v1/auth/sessions", None, None)
        .await
        .json();
    let phone_view = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == phone.to_string())
        .unwrap()
        .clone();
    assert_eq!(phone_view["approved"], json!([laptop]));
    let laptop_view = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == laptop.to_string())
        .unwrap()
        .clone();
    assert_eq!(laptop_view["approved_by"], phone.to_string());
    assert_eq!(laptop_view["method"], "qr");

    let response = hub
        .owner()
        .send(
            Method::DELETE,
            &format!("/api/v1/auth/sessions/{phone}"),
            None,
            None,
        )
        .await;
    let mut revoked: Vec<String> =
        serde_json::from_value(response.json::<Value>()["revoked"].clone()).unwrap();
    revoked.sort();
    let mut expected = vec![phone.to_string(), laptop.to_string(), tablet.to_string()];
    expected.sort();
    assert_eq!(revoked, expected);
    assert_eq!(hub.revoke_reason(phone).await.as_deref(), Some("owner"));
    assert_eq!(hub.revoke_reason(laptop).await.as_deref(), Some("cascade"));
    assert_eq!(hub.revoke_reason(tablet).await.as_deref(), Some("cascade"));
    assert_eq!(hub.revoke_reason(unrelated).await, None);
    assert_eq!(reason(&me(&hub, &laptop_token).await), "cascade");

    // Отзыв по бездействию и пределу не каскадирует.
    let (approver, _) = hub.session(SignInMethod::Password, None).await;
    let (child, _) = hub.session(SignInMethod::Qr, Some(approver)).await;
    sessions::revoke(
        &hub.pool,
        approver,
        canterlot_hub::owner_auth::RevokeReason::Idle,
        None,
    )
    .await
    .unwrap();
    assert_eq!(hub.revoke_reason(child).await, None);
    let (approver, _) = hub.session(SignInMethod::Password, None).await;
    let (child, _) = hub.session(SignInMethod::Qr, Some(approver)).await;
    sessions::revoke(
        &hub.pool,
        approver,
        canterlot_hub::owner_auth::RevokeReason::Limit,
        None,
    )
    .await
    .unwrap();
    assert_eq!(hub.revoke_reason(child).await, None);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_sessions_cascade_spares_the_acting_session(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (phone, _) = hub.session(SignInMethod::Password, None).await;
    // Текущий сеанс вошёл по QR с телефона, затем доказал пароль.
    sqlx::query("update owner_sessions set method='qr',approved_by=$1 where id=$2")
        .bind(phone)
        .bind(TEST_SESSION_ID)
        .execute(&hub.pool)
        .await
        .unwrap();
    let (child_of_current, _) = hub.session(SignInMethod::Qr, Some(TEST_SESSION_ID)).await;
    let response = hub
        .owner()
        .send(
            Method::DELETE,
            &format!("/api/v1/auth/sessions/{phone}"),
            None,
            None,
        )
        .await;
    assert_eq!(response.json::<Value>()["revoked"], json!([phone]));
    assert_eq!(hub.revoke_reason(TEST_SESSION_ID).await, None);
    assert_eq!(hub.revoke_reason(child_of_current).await, None);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_sessions_revoke_others_keeps_only_the_current(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (a, a_token) = hub.session(SignInMethod::Password, None).await;
    let (b, _) = hub.session(SignInMethod::Qr, Some(TEST_SESSION_ID)).await;
    let response = hub
        .owner()
        .send(
            Method::POST,
            "/api/v1/auth/sessions/revoke-others",
            None,
            None,
        )
        .await;
    assert_eq!(response.status, 200);
    let revoked = response.json::<Value>()["revoked"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(revoked, 2);
    assert_eq!(hub.revoke_reason(a).await.as_deref(), Some("others"));
    assert_eq!(hub.revoke_reason(b).await.as_deref(), Some("others"));
    assert_eq!(reason(&me(&hub, &a_token).await), "others");
    let list: Value = hub
        .owner()
        .send(Method::GET, "/api/v1/auth/sessions", None, None)
        .await
        .json();
    assert_eq!(list["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(list["current_id"], TEST_SESSION_ID.to_string());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_sessions_password_change_revokes_others(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    let (other, other_token) = hub.session(SignInMethod::Password, None).await;
    hub.make_stale(TEST_SESSION_ID).await;
    let response = hub
        .owner()
        .send(
            Method::PUT,
            "/api/v1/auth/password",
            None,
            Some(json!({"current_password": PASSWORD, "new_password": "the new password"})),
        )
        .await;
    assert_eq!(response.status, 200, "{:?}", response.body);
    assert_eq!(response.json::<Value>()["revoked"], json!([other]));
    assert_eq!(reason(&me(&hub, &other_token).await), "password_changed");
    let list: Value = hub
        .owner()
        .send(Method::GET, "/api/v1/auth/sessions", None, None)
        .await
        .json();
    assert!(
        list["fresh_until"].is_string(),
        "the change marks the session fresh"
    );
    let client = hub.anonymous(Some("203.0.113.70"));
    assert_eq!(hub.sign_in_owner(&client).await.status, 401);
    assert_eq!(
        hub.sign_in(&client, "owner", "the new password")
            .await
            .status,
        200
    );
    let short = hub
        .owner()
        .send(
            Method::PUT,
            "/api/v1/auth/password",
            None,
            Some(json!({"current_password": "the new password", "new_password": "short"})),
        )
        .await;
    assert_eq!(short.status, 400);
    assert_eq!(
        short.json::<Value>()["error"]["details"]["field"],
        "new_password"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_sessions_fifty_live_sessions_at_most(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let mut ids = Vec::new();
    for _ in 0..49 {
        ids.push(hub.session(SignInMethod::Password, None).await.0);
    }
    // Самый давний по активности — второй созданный.
    sqlx::query("update owner_sessions set last_seen_at=now()-interval '1 hour' where id=$1")
        .bind(ids[1])
        .execute(&hub.pool)
        .await
        .unwrap();
    let live = |pool: &PgPool| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(
                "select count(*) from owner_sessions where revoked_at is null",
            )
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(live(&hub.pool).await, 50);
    let created = sessions::create(
        &hub.pool,
        hub.owner_id(),
        SignInMethod::Password,
        None,
        &Default::default(),
    )
    .await
    .unwrap();
    assert_eq!(created.limited, vec![ids[1]]);
    assert_eq!(live(&hub.pool).await, 50);
    assert_eq!(hub.revoke_reason(ids[1]).await.as_deref(), Some("limit"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_sessions_last_seen_is_written_once_a_minute_or_on_ip_change(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (id, token) = hub.session(SignInMethod::Password, None).await;
    let at = |offset: &str| {
        let pool = hub.pool.clone();
        let offset = offset.to_owned();
        async move {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "update owner_sessions set last_seen_at=now()-interval '{offset}',last_ip='198.51.100.7' where id=$1"
            )))
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    let seen = || {
        let pool = hub.pool.clone();
        async move {
            sqlx::query_as::<_, (f64, Option<String>)>(
                "select extract(epoch from now()-last_seen_at)::float8,host(last_ip) from owner_sessions where id=$1",
            )
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    at("30 seconds").await;
    let response = me(&hub, &token).await;
    assert!(set_cookie(&response, SESSION_COOKIE).is_none());
    assert!(seen().await.0 >= 29.0);
    at("61 seconds").await;
    let response = me(&hub, &token).await;
    let (value, attributes) = set_cookie(&response, SESSION_COOKIE).expect("sliding cookie");
    assert_eq!(value, token);
    assert!(attributes.ends_with("Max-Age=7776000"));
    assert!(seen().await.0 < 5.0);
    at("10 seconds").await;
    hub.with_session(&token, Some("203.0.113.5"))
        .send(Method::GET, "/api/v1/me", None, None)
        .await;
    let (age, ip) = seen().await;
    assert!(age < 5.0);
    assert_eq!(ip.as_deref(), Some("203.0.113.5"));
    // Неизвестный адрес не стирает известный.
    at("61 seconds").await;
    hub.with_session(&token, None)
        .send(Method::GET, "/api/v1/me", None, None)
        .await;
    assert_eq!(seen().await.1.as_deref(), Some("198.51.100.7"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_sessions_list_rename_and_order(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    hub.set_password(PASSWORD).await;
    let signed = hub
        .sign_in_owner(
            &hub.anonymous(Some("203.0.113.80"))
                .with_header("User-Agent", "Mozilla/5.0 Chrome/142.0"),
        )
        .await;
    let token = session_token(&signed);
    let id: Uuid = signed.json::<Value>()["session"]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let old = hub.session(SignInMethod::Password, None).await.0;
    sqlx::query("update owner_sessions set last_seen_at=now()-interval '2 days' where id=$1")
        .bind(old)
        .execute(&hub.pool)
        .await
        .unwrap();
    let list: Value = hub
        .with_session(&token, Some("203.0.113.80"))
        .send(Method::GET, "/api/v1/auth/sessions", None, None)
        .await
        .json();
    let ids: Vec<&str> = list["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids[0], id.to_string(), "current first");
    assert_eq!(ids.last().copied(), Some(old.to_string()).as_deref());
    assert_eq!(list["current_id"], id.to_string());
    assert_eq!(list["idle_timeout_days"], 90);
    assert!(list["fresh_until"].is_string());
    let current = &list["sessions"][0];
    assert_eq!(current["current"], true);
    assert_eq!(current["label"], "Chrome 142");
    assert_eq!(current["user_agent"], "Mozilla/5.0 Chrome/142.0");
    assert_eq!(current["last_ip"], "203.0.113.80");

    let rename = |name: Value| {
        let client = hub.owner();
        async move {
            client
                .send(
                    Method::PATCH,
                    &format!("/api/v1/auth/sessions/{old}"),
                    None,
                    Some(json!({"name": name})),
                )
                .await
        }
    };
    let renamed = rename(json!("  Рабочий ноутбук  ")).await;
    assert_eq!(renamed.status, 200);
    assert_eq!(renamed.json::<Value>()["name"], "Рабочий ноутбук");
    assert_eq!(renamed.json::<Value>()["current"], false);
    assert_eq!(rename(json!("")).await.json::<Value>()["name"], Value::Null);
    assert_eq!(rename(json!("я".repeat(64))).await.status, 200);
    assert_eq!(rename(json!("я".repeat(65))).await.status, 400);
    assert_eq!(rename(json!("a\nb")).await.status, 400);
    let missing = hub
        .owner()
        .send(
            Method::PATCH,
            &format!("/api/v1/auth/sessions/{}", Uuid::now_v7()),
            None,
            Some(json!({"name": "x"})),
        )
        .await;
    assert_eq!(missing.status, 404);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn auth_sessions_hourly_maintenance_purges_auth_rows(pool: PgPool) {
    let hub = AuthHub::new(pool).await;
    let (old, token) = hub.session(SignInMethod::Password, None).await;
    sessions::revoke(
        &hub.pool,
        old,
        canterlot_hub::owner_auth::RevokeReason::Owner,
        None,
    )
    .await
    .unwrap();
    for sql in [
        "update owner_sessions set revoked_at=now()-interval '31 days' where revoke_reason is not null",
        "insert into auth_failures(bucket,at) values('ip:192.0.2.1',now()-interval '2 days')",
        "insert into auth_buckets(key,failures,last_failure_at) values('ip:192.0.2.1',3,now()-interval '2 days')",
        "insert into owner_recovery_tokens(owner_id,token_sha256,expires_at)
           select owner_id,sha256('x'::bytea),now()-interval '1 minute' from installation",
        "insert into owner_webauthn_challenges(id,owner_id,kind,bucket,state,expires_at)
           select gen_random_uuid(),owner_id,'sign_in','ip:192.0.2.1','{}',now()-interval '1 second'
           from installation",
        "insert into owner_webauthn_challenges(id,owner_id,kind,bucket,state,expires_at)
           select gen_random_uuid(),owner_id,'sign_in','ip:192.0.2.1','{}',now()+interval '1 minute'
           from installation",
        "insert into owner_qr_logins(id,owner_id,secret_sha256,poll_sha256,bucket,expires_at)
           select gen_random_uuid(),owner_id,sha256('a'::bytea),sha256('b'::bytea),'ip:192.0.2.1',
                  now()-interval '11 minutes' from installation",
        "insert into owner_qr_logins(id,owner_id,secret_sha256,poll_sha256,bucket,expires_at)
           select gen_random_uuid(),owner_id,sha256('c'::bytea),sha256('d'::bytea),'ip:192.0.2.1',
                  now()-interval '1 minute' from installation",
    ] {
        sqlx::query(sql).execute(&hub.pool).await.unwrap();
    }
    hub.state.scheduler.maintenance().await.unwrap();
    for (table, left) in [
        ("owner_sessions where revoked_at is not null", 0),
        ("owner_session_tombstones", 1),
        ("auth_failures", 0),
        ("auth_buckets", 0),
        ("owner_recovery_tokens", 0),
        ("owner_webauthn_challenges", 1),
        ("owner_qr_logins", 1),
    ] {
        let count: i64 =
            sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select count(*) from {table}")))
                .fetch_one(&hub.pool)
                .await
                .unwrap();
        assert_eq!(count, left, "{table}");
    }
    assert_eq!(reason(&me(&hub, &token).await), "owner");
}
