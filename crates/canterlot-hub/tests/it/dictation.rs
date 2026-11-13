//! Живая диктовка: словарь замен и WebSocket-пересылка службе речи.

use crate::support::task7 as support;

use axum::http::Method;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::Fixture;

fn term(pattern: &str, replacement: &str) -> Value {
    json!({"pattern": pattern, "replacement": replacement})
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terms_start_empty_and_survive_a_round_trip_in_order(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let body: Value = f.get("/api/v1/dictation/terms").await;
    assert_eq!(body, json!({"terms": []}));

    let terms = json!({"terms": [term("  мейн ", " main "), term("ветка", "branch")]});
    let response = f
        .client
        .send(
            Method::PUT,
            "/api/v1/dictation/terms",
            Some("dictation-terms-1"),
            Some(terms.clone()),
        )
        .await;
    assert_eq!(response.status, 200);
    let stored: Value = response.json();
    assert_eq!(
        stored,
        json!({"terms": [term("мейн", "main"), term("ветка", "branch")]})
    );
    let body: Value = f.get("/api/v1/dictation/terms").await;
    assert_eq!(body, stored);

    // Повтор с тем же ключом отдаёт сохранённое и ничего не добавляет.
    let again = f
        .client
        .send(
            Method::PUT,
            "/api/v1/dictation/terms",
            Some("dictation-terms-1"),
            Some(terms),
        )
        .await;
    assert_eq!(again.status, 200);
    assert_eq!(again.json::<Value>(), stored);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terms_put_replaces_the_whole_list_and_is_idempotent(pool: PgPool) {
    let f = Fixture::new(pool.clone()).await;
    let put = |key: &'static str, body: Value| {
        let client = f.client.clone();
        async move {
            client
                .send(
                    Method::PUT,
                    "/api/v1/dictation/terms",
                    Some(key),
                    Some(body),
                )
                .await
        }
    };
    assert_eq!(
        put(
            "dictation-a",
            json!({"terms": [term("раз", "one"), term("два", "two"), term("три", "three")]})
        )
        .await
        .status,
        200
    );
    let replaced = put("dictation-b", json!({"terms": [term("три", "three")]})).await;
    assert_eq!(replaced.status, 200);
    assert_eq!(
        replaced.json::<Value>(),
        json!({"terms": [term("три", "three")]})
    );
    // Замена целиком, а не слияние: старых строк не осталось.
    let rows: i64 = sqlx::query_scalar("select count(*) from dictation_terms where owner_id=$1")
        .bind(f.seed.owner_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 1);
    // Пустой список допустим и стирает всё.
    let cleared = put("dictation-c", json!({"terms": []})).await;
    assert_eq!(cleared.status, 200);
    let body: Value = f.get("/api/v1/dictation/terms").await;
    assert_eq!(body, json!({"terms": []}));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terms_validation_names_the_code_and_keeps_the_stored_list(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let good = json!({"terms": [term("мейн", "main")]});
    assert_eq!(
        f.client
            .send(
                Method::PUT,
                "/api/v1/dictation/terms",
                Some("dictation-good"),
                Some(good.clone()),
            )
            .await
            .status,
        200
    );

    let too_many: Vec<Value> = (0..201).map(|i| term(&format!("слово{i}"), "x")).collect();
    let long: String = "я".repeat(101);
    let cases: Vec<(Value, &str)> = vec![
        (json!({"terms": too_many}), "too_many_terms"),
        (json!({"terms": [term("   ", "main")]}), "term_empty"),
        (json!({"terms": [term("мейн", "")]}), "term_empty"),
        (json!({"terms": [term(&long, "x")]}), "term_too_long"),
        (json!({"terms": [term("мейн", &long)]}), "term_too_long"),
        (
            json!({"terms": [term("Мейн", "a"), term("мейн", "b")]}),
            "duplicate_term",
        ),
    ];
    for (index, (body, code)) in cases.into_iter().enumerate() {
        let response = f
            .client
            .send(
                Method::PUT,
                "/api/v1/dictation/terms",
                Some(&format!("dictation-bad-{index}")),
                Some(body),
            )
            .await;
        assert_eq!(response.status, 422, "{code}");
        let error: Value = response.json();
        assert_eq!(error["error"]["code"], code);
    }
    let body: Value = f.get("/api/v1/dictation/terms").await;
    assert_eq!(body, good);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn terms_put_requires_an_idempotency_key_and_rejects_its_reuse(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let body = json!({"terms": [term("мейн", "main")]});
    let without = f
        .client
        .send(
            Method::PUT,
            "/api/v1/dictation/terms",
            None,
            Some(body.clone()),
        )
        .await;
    assert_eq!(without.status, 400);

    assert_eq!(
        f.client
            .send(
                Method::PUT,
                "/api/v1/dictation/terms",
                Some("dictation-same"),
                Some(body.clone())
            )
            .await
            .status,
        200
    );
    let conflict = f
        .client
        .send(
            Method::PUT,
            "/api/v1/dictation/terms",
            Some("dictation-same"),
            Some(json!({"terms": [term("ветка", "branch")]})),
        )
        .await;
    assert_eq!(conflict.status, 409);
    let error: Value = conflict.json();
    assert_eq!(error["error"]["code"], "idempotency_conflict");
}
