//! Заморозка контракта для плана 1.3b: таблицы `docs/design/api.md` сверяются с
//! `docs/api/openapi.json` и `docs/api/events.schema.json`, каждый изменяющий маршрут
//! документа OpenAPI записывает ключ идемпотентности ровно один раз.

use std::collections::{BTreeMap, BTreeSet};

use axum::http::Method;
use canterlot_hub::{
    events::CLIENT_KINDS,
    http::{self, AppState, error::ErrorCode, openapi},
    testkit::{self, TestClient, TestResponse},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

fn repo_file(path: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path),
    )
    .unwrap_or_else(|_| panic!("{path} is committed"))
}

fn openapi_document() -> Value {
    let committed = repo_file("docs/api/openapi.json");
    assert!(
        committed == openapi::render(),
        "docs/api/openapi.json is stale: run scripts/gen-openapi.sh"
    );
    serde_json::from_str(&committed).unwrap()
}

/// Операции документа: (метод в верхнем регистре, шаблон пути).
fn operations(document: &Value) -> BTreeSet<(String, String)> {
    document["paths"]
        .as_object()
        .unwrap()
        .iter()
        .flat_map(|(path, item)| {
            item.as_object()
                .unwrap()
                .keys()
                .filter(|method| *method != "parameters")
                .map(|method| (method.to_uppercase(), path.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Раздел `## <title>` файла api.md до следующего раздела того же уровня.
fn section<'a>(document: &'a str, title: &str) -> &'a str {
    document
        .split(&format!("\n## {title}\n"))
        .nth(1)
        .unwrap_or_else(|| panic!("section {title} in api.md"))
        .split("\n## ")
        .next()
        .unwrap()
}

fn cells(line: &str) -> Vec<&str> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

fn spans(cell: &str) -> Vec<&str> {
    cell.split('`').skip(1).step_by(2).collect()
}

fn is_name(span: &str) -> bool {
    !span.is_empty() && span.chars().all(|c| c.is_ascii_lowercase() || c == '_')
}

const METHODS: [&str; 6] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"];

#[test]
fn distinct_rust_views_publish_unique_schema_names() {
    use utoipa::ToSchema;

    let document = openapi_document();
    let mut names = BTreeMap::new();
    macro_rules! register {
        ($($ty:ty),+ $(,)?) => {$({
            let name = <$ty>::name();
            let previous = names.insert(name.clone(), std::any::type_name::<$ty>());
            assert!(previous.is_none(), "schema {name} is shared by {previous:?} and {}", std::any::type_name::<$ty>());
            assert_eq!(
                document["components"]["schemas"][name.as_ref()],
                serde_json::to_value(<$ty as utoipa::PartialSchema>::schema()).unwrap(),
                "published schema for {}", std::any::type_name::<$ty>()
            );
        })+};
    }
    register!(
        canterlot_hub::attachments::AttachmentView,
        canterlot_hub::history::AttachmentView,
        canterlot_hub::conversations::ActivityView,
        canterlot_hub::http::health::ActivityView,
        canterlot_hub::conversations::QueueView,
        canterlot_hub::snapshot::QueueView,
        canterlot_hub::http::conversations::DecisionBody,
        canterlot_hub::http::conversations::DecisionOutcomeView,
        canterlot_hub::http::conversations::DecisionAnswer,
        canterlot_hub::http::conversations::DecisionBehavior,
    );
}

#[test]
fn coverage_table_matches_openapi_and_event_schema() {
    let api_md = repo_file("docs/design/api.md");
    let document = openapi_document();
    let schema: Value = serde_json::from_str(&repo_file("docs/api/events.schema.json")).unwrap();

    let schema_kinds: BTreeSet<String> = schema["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["$ref"]
                .as_str()
                .unwrap()
                .strip_prefix("#/$defs/")
                .unwrap()
                .to_owned()
        })
        .collect();
    let defined: BTreeSet<String> = schema["$defs"]
        .as_object()
        .unwrap()
        .keys()
        .filter(|k| is_name(k))
        .cloned()
        .collect();
    assert_eq!(schema_kinds, defined, "every event $def is in oneOf");
    let client: BTreeSet<String> = CLIENT_KINDS.iter().map(|k| (*k).to_owned()).collect();
    assert_eq!(schema_kinds, client, "events.schema.json and CLIENT_KINDS");
    let ws_kinds: BTreeSet<String> = schema["$defs"]["ServerOp"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|op| op["properties"]["op"]["const"] == "event")
        .unwrap()["properties"]["event"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["$ref"]
                .as_str()
                .unwrap()
                .strip_prefix("#/$defs/")
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(ws_kinds, client, "ServerOp.event and CLIENT_KINDS");
    let server_ops: BTreeSet<String> = schema["$defs"]["ServerOp"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op| op["properties"]["op"]["const"].as_str().unwrap().to_owned())
        .collect();

    // Таблица видов событий: ровно клиентские виды, у каждого область.
    let mut kinds_table = BTreeSet::new();
    for line in section(&api_md, "Виды событий")
        .lines()
        .filter(|l| l.starts_with("| "))
    {
        let row = cells(line);
        let Some(kind) = spans(row[1])
            .first()
            .copied()
            .filter(|k| is_name(k) && *k != "kind")
        else {
            continue;
        };
        assert!(
            !row[0].is_empty() && !row[2].is_empty(),
            "{kind}: scope and purpose"
        );
        kinds_table.insert(kind.to_owned());
    }
    assert_eq!(kinds_table, client, "event-kind table in api.md");

    // Таблица покрытия экранов: маршруты и виды событий в обе стороны.
    let mut routes = BTreeSet::new();
    let mut kinds = BTreeSet::new();
    let mut rows = 0;
    for line in section(&api_md, "Покрытие экранов пилота").lines() {
        let row = cells(line);
        if row.len() != 5 || row[0].parse::<u32>().is_err() {
            continue;
        }
        rows += 1;
        assert!(!row[1].is_empty() && !row[4].is_empty(), "row {}", row[0]);
        for span in spans(row[2]) {
            let Some((method, path)) = span.split_once(' ') else {
                continue;
            };
            if METHODS.contains(&method) {
                let path = path.split('?').next().unwrap();
                routes.insert((method.to_owned(), path.to_owned()));
            }
        }
        for span in spans(row[3]) {
            assert!(is_name(span), "row {}: {span} is not a kind", row[0]);
            if !schema_kinds.contains(span) {
                assert!(
                    server_ops.contains(span),
                    "row {}: {span} is neither an event kind nor a ServerOp",
                    row[0]
                );
                continue;
            }
            kinds.insert(span.to_owned());
        }
    }
    assert_eq!(rows, 31, "screen coverage rows");
    let in_document = operations(&document);
    let missing: Vec<_> = in_document.difference(&routes).collect();
    let unknown: Vec<_> = routes.difference(&in_document).collect();
    assert!(
        missing.is_empty(),
        "operations without a coverage row: {missing:?}"
    );
    assert!(
        unknown.is_empty(),
        "coverage rows name unknown routes: {unknown:?}"
    );
    let missing: Vec<_> = schema_kinds.difference(&kinds).collect();
    assert!(
        missing.is_empty(),
        "event kinds without a coverage row: {missing:?}"
    );

    // Таблица ошибок: каждый `ErrorCode` перечисления и OpenAPI.
    let table: BTreeSet<String> = section(&api_md, "Errors")
        .lines()
        .filter(|l| l.starts_with("| `") && !l.starts_with("| `code`"))
        .map(|l| spans(cells(l)[0])[0].to_owned())
        .collect();
    let enumerated: BTreeSet<String> = ErrorCode::ALL
        .iter()
        .map(|c| c.as_str().to_owned())
        .collect();
    let documented: BTreeSet<String> = document["components"]["schemas"]["ErrorCode"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(table, enumerated, "error table and ErrorCode");
    assert_eq!(documented, enumerated, "OpenAPI ErrorCode and the enum");
}

// ---------------------------------------------------------------- идемпотентность всего API

struct Case {
    method: Method,
    template: &'static str,
    path: String,
    body: Option<Value>,
    /// Другое тело того же маршрута; без тела ключ повторяется на другом маршруте.
    changed: Option<Value>,
}

fn case(
    method: Method,
    template: &'static str,
    path: String,
    body: Option<Value>,
    changed: Option<Value>,
) -> Case {
    Case {
        method,
        template,
        path,
        body,
        changed,
    }
}

const SHA_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

/// Маршруты, освобождённые от ключа: загрузка части (идемпотентна по смещению), отметка прочтения,
/// просьба обновить лимиты (повтор безвреден, частоту ограничивает хаб), полная замена
/// черновика (тот же текст и список вложений дают тот же результат) и чтение телом POST:
/// предпросмотр маршрута и поиск по истории (запрос не должен попадать в адрес).
fn exempt(method: &str, path: &str) -> bool {
    (method == "PATCH" && path.ends_with("/content"))
        || (method == "POST" && path.ends_with("/read"))
        || (method == "POST" && path == "/api/v1/limits/refresh")
        || (method == "POST" && path == "/api/v1/routing-policy/preview")
        || (method == "POST" && path == "/api/v1/search")
        || (method == "PUT" && path.ends_with("/draft"))
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn every_mutating_endpoint_records_its_key_once_across_the_whole_api(pool: PgPool) {
    let (deps, _dir) =
        testkit::test_app_deps(pool.clone(), testkit::test_http_config(), None).await;
    let owner = deps.installation.owner_id;
    let machine = Uuid::now_v7();
    let send_now_registry = deps.registry.clone();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'test')")
        .bind(machine)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    testkit::seed_catalog(&pool, machine).await;
    let id = testkit::seed_conversation(&pool, owner, machine).await;
    let doomed = testkit::seed_conversation(&pool, owner, machine).await;
    let resume_id = testkit::seed_conversation(&pool, owner, machine).await;
    let stuck = testkit::seed_conversation(&pool, owner, machine).await;
    let delegated = testkit::seed_conversation(&pool, owner, machine).await;
    let trashed = testkit::seed_conversation(&pool, owner, machine).await;
    let restore_me = testkit::seed_conversation(&pool, owner, machine).await;
    sqlx::query("update conversations set trashed_at=clock_timestamp() where id=any($1)")
        .bind(&[trashed, restore_me][..])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update conversations set parent_conversation_id=$2,spawn_depth=1 where id=$1")
        .bind(delegated)
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update conversations set model='synthetic-small',effort='high' where id=$1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let live = canterlot_hub::executions::start(&pool, owner, stuck, Uuid::now_v7())
        .await
        .unwrap();
    use canterlot_hub::model::ExecutionState;
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        canterlot_hub::executions::set_state(&pool, live.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    sqlx::query("update queue_state set pause_reasons=$2 where conversation_id=$1")
        .bind(stuck)
        .bind(json!([{"reason":"session_stuck"}]))
        .execute(&pool)
        .await
        .unwrap();
    let client = TestClient::in_process(http::router(AppState::new(deps))).as_owner();
    let setup = async |method: Method, path: String, key: &str, body: Option<Value>| -> Value {
        let response = client.send(method, &path, Some(key), body).await;
        assert!(
            response.status.is_success(),
            "setup {path}: {}",
            response.status
        );
        response.json()
    };
    let group = setup(
        Method::POST,
        "/api/v1/groups".into(),
        "setup_group",
        Some(json!({"name":"Группа"})),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let preset = setup(
        Method::POST,
        "/api/v1/presets".into(),
        "setup_preset",
        Some(json!({"title":"Пресет покрытия","text":"текст"})),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let preset_patch = setup(
        Method::POST,
        "/api/v1/presets".into(),
        "setup_preset_patch",
        Some(json!({"title":"Пресет правки","text":"текст"})),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let input = setup(
        Method::POST,
        format!("/api/v1/conversations/{id}/inputs"),
        "setup_input",
        Some(json!({"content":[{"type":"text","text":"Первое"}]})),
    )
    .await["input_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let attachment_body =
        |name: &str| json!({"conversation_id":id,"name":name,"size":3,"sha256":SHA_ABC});
    let mut attachments = Vec::new();
    for (key, name) in [("setup_upload", "abc.txt"), ("setup_cancel", "cancel.txt")] {
        let attachment = setup(
            Method::POST,
            "/api/v1/attachments".into(),
            key,
            Some(attachment_body(name)),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let uploaded = client
            .send_body(
                Method::PATCH,
                &format!("/api/v1/attachments/{attachment}/content"),
                &[("upload-offset", "0".into())],
                axum::body::Body::from("abc"),
            )
            .await;
        assert_eq!(uploaded.status, 204);
        attachments.push(attachment);
    }
    let (uploaded, to_cancel) = (attachments[0].clone(), attachments[1].clone());
    let run_at = |hours: i64| {
        (chrono::Utc::now() + chrono::TimeDelta::hours(hours))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    };
    let scheduled_body = |hours: i64, text: &str| json!({"run_at":run_at(hours),"content":[{"type":"text","text":text}]});
    let mut scheduled = Vec::new();
    for key in ["setup_scheduled_patch", "setup_scheduled_delete"] {
        scheduled.push(
            setup(
                Method::POST,
                format!("/api/v1/conversations/{id}/scheduled"),
                key,
                Some(scheduled_body(1, "Позже")),
            )
            .await["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let alarm_id = Uuid::now_v7();
    sqlx::query(
        "insert into alarms(id,owner_id,conversation_id,created_by,kind,fire_at,call_id)
         values($1,$2,$3,'agent','at',now()+interval '1 hour',$4)",
    )
    .bind(alarm_id)
    .bind(owner)
    .bind(id)
    .bind(Uuid::now_v7())
    .execute(&pool)
    .await
    .unwrap();
    let queued_spawn = Uuid::now_v7();
    let started_spawn = Uuid::now_v7();
    for (queue_id, state, child) in [
        (queued_spawn, "queued", None),
        (started_spawn, "started", Some(delegated)),
    ] {
        sqlx::query(
            "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,not_before,state,child_id)
             values($1,$2,$3,$4,'{\"chain_depth\":0,\"args\":{\"brief\":\"b\"}}','rate',now(),$5,$6)",
        )
        .bind(queue_id)
        .bind(owner)
        .bind(id)
        .bind(Uuid::now_v7())
        .bind(state)
        .bind(child)
        .execute(&pool)
        .await
        .unwrap();
    }
    // «Отправить сейчас»: ход идёт, один ввод принят, второй ждёт в очереди.
    let send_now_conversation = testkit::seed_conversation(&pool, owner, machine).await;
    sqlx::query("update conversations set harness = 'codex' where id = $1")
        .bind(send_now_conversation)
        .execute(&pool)
        .await
        .unwrap();
    let _send_now_caps = send_now_registry.connect_for_tests(
        machine,
        &[
            "pilot_v1",
            "instructions_v1",
            "todo_v1",
            "compact_v1",
            "background_v1",
            "session_tools_v1",
            "owner_exit_v1",
            "workdir_suggest_v1",
            "owner_instructions_v1",
            "send_now_v1",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>(),
    );
    let send_now_execution =
        canterlot_hub::executions::start(&pool, owner, send_now_conversation, Uuid::now_v7())
            .await
            .unwrap()
            .execution_id;
    {
        use canterlot_hub::model::ExecutionState as State;
        for state in [State::Starting, State::Running] {
            canterlot_hub::executions::set_state(
                &pool,
                send_now_execution,
                state,
                None,
                Uuid::now_v7(),
            )
            .await
            .unwrap();
        }
    }
    let first = setup(
        Method::POST,
        format!("/api/v1/conversations/{send_now_conversation}/inputs"),
        "setup_send_now_first",
        Some(json!({"content":[{"type":"text","text":"Ход"}]})),
    )
    .await;
    let queued = setup(
        Method::POST,
        format!("/api/v1/conversations/{send_now_conversation}/inputs"),
        "setup_send_now_queued",
        Some(json!({"content":[{"type":"text","text":"В очередь"}]})),
    )
    .await;
    {
        use canterlot_hub::delivery::{
            dispatch::{GateInputs, dispatch_next},
            receipts::{InputReceipt, apply_input_receipt},
        };
        let outcome = dispatch_next(
            &pool,
            send_now_conversation,
            GateInputs {
                machine_available: true,
                machine_reconciled: true,
                session_capabilities: vec!["pilot_v1".into(), "send_now_v1".into()],
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        assert!(matches!(
            outcome,
            canterlot_hub::delivery::dispatch::DispatchOutcome::Dispatched { .. }
        ));
        for receipt in [
            InputReceipt::Stored,
            InputReceipt::TransmissionStarted,
            InputReceipt::Accepted,
        ] {
            apply_input_receipt(
                &pool,
                first["command_id"].as_str().unwrap().parse().unwrap(),
                1,
                receipt,
                Uuid::now_v7(),
            )
            .await
            .unwrap();
        }
    }
    let edit_input = setup(
        Method::POST,
        format!("/api/v1/conversations/{id}/inputs"),
        "setup_edit_input",
        Some(json!({"content":[{"type":"text","text":"Черновик"}]})),
    )
    .await["input_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let decision_conversation = testkit::seed_conversation(&pool, owner, machine).await;
    let decision_execution = Uuid::now_v7();
    let decision_request = Uuid::now_v7();
    sqlx::query(
        "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'running')",
    )
    .bind(decision_execution)
    .bind(decision_conversation)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("insert into requests(id,conversation_id,execution_id,generation,kind,native_request_id,state,request_payload) values($1,$2,$3,1,'permission','coverage','open','{}')")
        .bind(decision_request).bind(decision_conversation).bind(decision_execution).execute(&pool).await.unwrap();
    let cases = vec![
        case(
            Method::POST,
            "/api/v1/conversations",
            "/api/v1/conversations".into(),
            Some(
                json!({"machine_id":machine,"harness":"synthetic","workdir":"/tmp/a","model":"synthetic-small","effort":"high"}),
            ),
            Some(
                json!({"machine_id":machine,"harness":"synthetic","workdir":"/tmp/b","model":"synthetic-small","effort":"high"}),
            ),
        ),
        case(
            Method::PATCH,
            "/api/v1/conversations/{id}",
            format!("/api/v1/conversations/{id}"),
            Some(json!({"color":"red"})),
            Some(json!({"color":"blue"})),
        ),
        case(
            Method::PUT,
            "/api/v1/conversations/{id}/groups",
            format!("/api/v1/conversations/{id}/groups"),
            Some(json!({"group_ids":[group]})),
            Some(json!({"group_ids":[]})),
        ),
        case(
            Method::PUT,
            "/api/v1/conversations/{id}/groups/{group_id}/pin",
            format!("/api/v1/conversations/{id}/groups/{group}/pin"),
            None,
            None,
        ),
        case(
            Method::DELETE,
            "/api/v1/conversations/{id}/groups/{group_id}/pin",
            format!("/api/v1/conversations/{id}/groups/{group}/pin"),
            None,
            None,
        ),
        case(
            Method::PUT,
            "/api/v1/conversations/{id}/nudge",
            format!("/api/v1/conversations/{id}/nudge"),
            Some(json!({"idle_minutes":5})),
            Some(json!({"paused":true})),
        ),
        case(
            Method::DELETE,
            "/api/v1/conversations/{id}/nudge",
            format!("/api/v1/conversations/{id}/nudge"),
            None,
            None,
        ),
        case(
            Method::PUT,
            "/api/v1/conversations/{id}/crunch",
            format!("/api/v1/conversations/{id}/crunch"),
            Some(json!({"enabled":true})),
            Some(json!({"enabled":false})),
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/accept",
            format!("/api/v1/conversations/{delegated}/accept"),
            Some(json!({"force":false})),
            Some(json!({"force":true})),
        ),
        case(
            Method::PUT,
            "/api/v1/spawn-whitelist",
            "/api/v1/spawn-whitelist".into(),
            Some(json!({"entries":[{"harness":"codex","model":"default"}]})),
            Some(json!({"entries":null})),
        ),
        case(
            Method::PUT,
            "/api/v1/routing-policy",
            "/api/v1/routing-policy".into(),
            Some(json!({"policy":null,"revision":0})),
            Some(
                json!({"policy":{"task_types":{"smoke":{"title":"Проверка","candidates":[{"harness":"opencode","model":"m"}]}}},"revision":0}),
            ),
        ),
        case(
            Method::PUT,
            "/api/v1/limit-settings",
            "/api/v1/limit-settings".into(),
            Some(
                json!({"settings":{"warn_remaining_pct":20,"danger_remaining_pct":5,"reserve_remaining_pct":10}}),
            ),
            Some(json!({"settings":null})),
        ),
        case(
            Method::POST,
            "/api/v1/groups",
            "/api/v1/groups".into(),
            Some(json!({"name":"Другая"})),
            Some(json!({"name":"Третья"})),
        ),
        case(
            Method::PATCH,
            "/api/v1/groups/{id}",
            format!("/api/v1/groups/{group}"),
            Some(json!({"position":5})),
            Some(json!({"position":6})),
        ),
        case(
            Method::DELETE,
            "/api/v1/groups/{id}",
            format!("/api/v1/groups/{group}"),
            None,
            None,
        ),
        case(
            Method::POST,
            "/api/v1/presets",
            "/api/v1/presets".into(),
            Some(json!({"title":"Ещё пресет","text":"текст"})),
            Some(json!({"title":"Иной пресет","text":"текст"})),
        ),
        case(
            Method::PATCH,
            "/api/v1/presets/{id}",
            format!("/api/v1/presets/{preset_patch}"),
            Some(json!({"title":"Новое название","base_revision":1})),
            Some(json!({"title":"Другое название","base_revision":1})),
        ),
        case(
            Method::DELETE,
            "/api/v1/presets/{id}",
            format!("/api/v1/presets/{preset}"),
            None,
            None,
        ),
        case(
            Method::PATCH,
            "/api/v1/conversations/{id}/settings",
            format!("/api/v1/conversations/{id}/settings"),
            Some(json!({"effort":"low"})),
            Some(json!({"effort":"high"})),
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/inputs",
            format!("/api/v1/conversations/{id}/inputs"),
            Some(json!({"content":[{"type":"text","text":"Следующее"}]})),
            Some(json!({"content":[{"type":"text","text":"Другое"}]})),
        ),
        case(
            Method::POST,
            "/api/v1/inputs/{id}/cancel",
            format!("/api/v1/inputs/{input}/cancel"),
            None,
            None,
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/interrupt",
            format!("/api/v1/conversations/{id}/interrupt"),
            None,
            None,
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/inputs/{input_id}/send-now",
            format!(
                "/api/v1/conversations/{send_now_conversation}/inputs/{}/send-now",
                queued["input_id"].as_str().unwrap()
            ),
            None,
            None,
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/compact",
            format!("/api/v1/conversations/{stuck}/compact"),
            None,
            None,
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/stop",
            format!("/api/v1/conversations/{id}/stop"),
            Some(json!({"force":false})),
            Some(json!({"force":true})),
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/queue/continue",
            format!("/api/v1/conversations/{id}/queue/continue"),
            None,
            None,
        ),
        case(
            Method::PATCH,
            "/api/v1/conversations/{id}/inputs/{input_id}",
            format!("/api/v1/conversations/{id}/inputs/{edit_input}"),
            Some(json!({"text":"Исправлено","expected_revision":1})),
            Some(json!({"text":"Исправлено ещё раз","expected_revision":2})),
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/resolve-owner-lost",
            format!("/api/v1/conversations/{id}/resolve-owner-lost"),
            None,
            None,
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/executions",
            format!("/api/v1/conversations/{id}/executions"),
            None,
            None,
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/spend-reserve",
            format!("/api/v1/conversations/{id}/spend-reserve"),
            None,
            None,
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/resume",
            format!("/api/v1/conversations/{resume_id}/resume"),
            Some(json!({"continue_queue":false})),
            Some(json!({"continue_queue":true})),
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/scheduled",
            format!("/api/v1/conversations/{id}/scheduled"),
            Some(scheduled_body(2, "Через два часа")),
            Some(scheduled_body(3, "Через три часа")),
        ),
        case(
            Method::PATCH,
            "/api/v1/scheduled/{id}",
            format!("/api/v1/scheduled/{}", scheduled[0]),
            Some(json!({"run_at":run_at(4)})),
            Some(json!({"run_at":run_at(5)})),
        ),
        case(
            Method::DELETE,
            "/api/v1/scheduled/{id}",
            format!("/api/v1/scheduled/{}", scheduled[1]),
            None,
            None,
        ),
        case(
            Method::DELETE,
            "/api/v1/conversations/{id}/alarms/{alarm_id}",
            format!("/api/v1/conversations/{id}/alarms/{alarm_id}"),
            None,
            None,
        ),
        case(
            Method::DELETE,
            "/api/v1/conversations/{id}/spawn-queue/{queue_id}",
            format!("/api/v1/conversations/{id}/spawn-queue/{queued_spawn}"),
            None,
            None,
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/spawn-queue/{queue_id}/start-now",
            format!("/api/v1/conversations/{id}/spawn-queue/{started_spawn}/start-now"),
            None,
            None,
        ),
        case(
            Method::POST,
            "/api/v1/attachments",
            "/api/v1/attachments".into(),
            Some(attachment_body("one.txt")),
            Some(attachment_body("two.txt")),
        ),
        case(
            Method::POST,
            "/api/v1/attachments/{id}/complete",
            format!("/api/v1/attachments/{uploaded}/complete"),
            None,
            Some(json!({"sha256":SHA_ABC})),
        ),
        case(
            Method::DELETE,
            "/api/v1/attachments/{id}",
            format!("/api/v1/attachments/{to_cancel}"),
            None,
            None,
        ),
        case(
            Method::DELETE,
            "/api/v1/conversations/{id}",
            format!("/api/v1/conversations/{doomed}"),
            None,
            None,
        ),
        case(
            Method::DELETE,
            "/api/v1/conversations/{id}/trash",
            format!("/api/v1/conversations/{trashed}/trash"),
            None,
            None,
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/restore",
            format!("/api/v1/conversations/{restore_me}/restore"),
            None,
            None,
        ),
        case(
            Method::POST,
            "/api/v1/conversations/{id}/requests/{request_id}/decision",
            format!(
                "/api/v1/conversations/{decision_conversation}/requests/{decision_request}/decision"
            ),
            Some(json!({"kind":"permission","behavior":"allow","message":null})),
            Some(json!({"kind":"permission","behavior":"deny","message":null})),
        ),
    ];

    let mutating: BTreeSet<(String, String)> = operations(&openapi_document())
        .into_iter()
        .filter(|(method, path)| method != "GET" && method != "HEAD" && !exempt(method, path))
        .collect();
    let fixtures: BTreeSet<(String, String)> = cases
        .iter()
        .map(|c| (c.method.as_str().to_owned(), c.template.to_owned()))
        .collect();
    assert_eq!(fixtures.len(), cases.len(), "one fixture per route");
    let without: Vec<_> = mutating.difference(&fixtures).collect();
    assert!(
        without.is_empty(),
        "mutating routes without a fixture: {without:?}"
    );
    let stale: Vec<_> = fixtures.difference(&mutating).collect();
    assert!(
        stale.is_empty(),
        "fixtures for routes not in openapi.json: {stale:?}"
    );

    let keys = async |key: &str| -> i64 {
        sqlx::query_scalar("select count(*) from idempotency_keys where owner_id=$1 and key=$2")
            .bind(owner)
            .bind(key)
            .fetch_one(&pool)
            .await
            .unwrap()
    };
    let error_code = |r: &TestResponse| r.json::<Value>()["error"]["code"].clone();
    let mut checked = BTreeMap::new();
    for (i, c) in cases.into_iter().enumerate() {
        let key = format!("whole_api_{i}");
        let route = format!("{} {}", c.method, c.template);
        let first = client
            .send(c.method.clone(), &c.path, Some(&key), c.body.clone())
            .await;
        let after_first = keys(&key).await;
        let replay = client
            .send(c.method.clone(), &c.path, Some(&key), c.body.clone())
            .await;
        let after_replay = keys(&key).await;
        assert!(first.status.is_success(), "{route}: {}", first.status);
        assert_eq!(after_first, 1, "{route}: key after the first call");
        assert_eq!(replay.status, first.status, "{route}");
        assert_eq!(replay.body, first.body, "{route}: replay is byte-identical");
        assert_eq!(after_replay, 1, "{route}: key after the replay");
        let conflict = match c.changed {
            Some(changed) => {
                client
                    .send(c.method.clone(), &c.path, Some(&key), Some(changed))
                    .await
            }
            None => {
                let other = if c.template.ends_with("/interrupt") {
                    format!("/api/v1/conversations/{id}/queue/continue")
                } else {
                    format!("/api/v1/conversations/{id}/interrupt")
                };
                client.send(Method::POST, &other, Some(&key), None).await
            }
        };
        assert_eq!(conflict.status, 409, "{route}: {:?}", conflict.body);
        assert_eq!(error_code(&conflict), "idempotency_conflict", "{route}");
        assert_eq!(keys(&key).await, 1, "{route}: key after the conflict");
        checked.insert(route, first.status.as_u16());
    }
    assert_eq!(checked.len(), mutating.len());
}

#[test]
fn openapi_documents_global_server_time() {
    let document = serde_json::to_value(openapi::document()).unwrap();
    for (path, item) in document["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            if let Some(responses) = operation["responses"].as_object() {
                for (status, response) in responses {
                    assert_eq!(
                        response["headers"]["X-Server-Time-Ms"]["schema"]["type"], "integer",
                        "{method} {path} {status}: server time header"
                    );
                }
            }
        }
    }
}

#[test]
fn openapi_path_parameters_appear_in_their_path() {
    let document = serde_json::to_value(openapi::document()).unwrap();
    let mut stray = Vec::new();
    for (path, item) in document["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            for parameter in operation["parameters"].as_array().into_iter().flatten() {
                let name = parameter["name"].as_str().unwrap();
                if parameter["in"] == "path" && !path.contains(&format!("{{{name}}}")) {
                    stray.push(format!("{method} {path}: {name}"));
                }
            }
        }
    }
    assert!(
        stray.is_empty(),
        "path parameters missing from the path: {stray:#?}"
    );
}

#[test]
fn openapi_documents_optional_history_query() {
    let document = serde_json::to_value(openapi::document()).unwrap();
    let parameters = document["paths"]["/api/v1/conversations/{id}/history"]["get"]["parameters"]
        .as_array()
        .unwrap();
    for name in ["before", "after"] {
        let parameter = parameters.iter().find(|p| p["name"] == name).unwrap();
        assert_eq!(parameter["in"], "query", "{name}");
        assert_ne!(parameter["required"], true, "{name}");
    }
}

#[test]
fn openapi_documents_lane_page_with_optional_query() {
    let document = serde_json::to_value(openapi::document()).unwrap();
    let operation = &document["paths"]["/api/v1/conversations/{id}/lanes/{block_id}"]["get"];
    let parameters = operation["parameters"].as_array().unwrap();
    for name in ["before", "limit"] {
        let parameter = parameters.iter().find(|p| p["name"] == name).unwrap();
        assert_eq!(parameter["in"], "query", "{name}");
        assert_ne!(parameter["required"], true, "{name}");
    }
    let schema = &document["components"]["schemas"]["LanePage"];
    for field in ["root", "items"] {
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!(field)),
            "{field}"
        );
    }
}

#[test]
fn openapi_operation_ids_are_unique_for_type_generation() {
    let document = serde_json::to_value(openapi::document()).unwrap();
    let mut seen = BTreeSet::new();
    for item in document["paths"].as_object().unwrap().values() {
        for operation in item.as_object().unwrap().values() {
            if let Some(id) = operation["operationId"].as_str() {
                assert!(seen.insert(id), "duplicate operationId: {id}");
            }
        }
    }
}
