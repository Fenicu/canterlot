//! Тесты задачи 4 плана 2c: вызовы инструментов агента в хабе.
//!
//! Агент действует только на свою сессию: разговор и владелец выводятся
//! из живого исполнения и поколения на вызвавшей машине, а не из аргументов.

use canterlot_hub::{
    config::HubTimers,
    conversations,
    executions::{self, StartedExecution},
    grpc::HubGrpc,
    installation, machines, presets,
    session::Registry,
    testkit,
};
use canterlot_proto::worker::v1::{AgentCallRequest, worker_data_server::WorkerData};
use serde_json::{Value, json};
use sqlx::PgPool;
use tonic::Request;
use uuid::Uuid;

struct Fx {
    pool: PgPool,
    grpc: HubGrpc,
    owner: Uuid,
    machine: Uuid,
    token: String,
    conversation: Uuid,
    execution: StartedExecution,
    _dir: tempfile::TempDir,
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let installation = installation::init(&pool, &dir.path().join("manifest"))
            .await
            .unwrap();
        let owner = installation.owner_id;
        let machine = machines::add(&pool, owner, "synthetic").await.unwrap();
        testkit::seed_catalog(&pool, machine.machine_id).await;
        let conversation = testkit::seed_conversation(&pool, owner, machine.machine_id).await;
        sqlx::query("update conversations set model='synthetic-small',effort='high' where id=$1")
            .bind(conversation)
            .execute(&pool)
            .await
            .unwrap();
        let execution = executions::start(&pool, owner, conversation, Uuid::now_v7())
            .await
            .unwrap();
        let timers = HubTimers::default();
        let grpc = HubGrpc::new(
            pool.clone(),
            Registry::new(timers.clone()),
            installation,
            timers,
        );
        Self {
            pool,
            grpc,
            owner,
            machine: machine.machine_id,
            token: machine.token,
            conversation,
            execution,
            _dir: dir,
        }
    }

    fn request(
        &self,
        tool: &str,
        args: Value,
        call_id: &Uuid,
        execution: Option<(Uuid, i64)>,
        token: Option<&str>,
    ) -> Request<AgentCallRequest> {
        let (execution_id, generation) =
            execution.unwrap_or((self.execution.execution_id, self.execution.generation));
        let mut request = Request::new(AgentCallRequest {
            execution_id: execution_id.to_string(),
            generation: generation as u64,
            call_id: call_id.to_string(),
            tool: tool.to_owned(),
            arguments_json: args.to_string(),
        });
        let token = token.unwrap_or(&self.token);
        request
            .metadata_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        request
    }

    async fn agent(
        &self,
        tool: &str,
        args: Value,
        call_id: &Uuid,
    ) -> canterlot_proto::worker::v1::AgentCallResponse {
        let result =
            WorkerData::agent_call(&self.grpc, self.request(tool, args, call_id, None, None)).await;
        assert!(result.is_ok(), "вызов агента должен отвечать: {result:?}");
        result.unwrap().into_inner()
    }

    async fn create_preset(&self, title: &str, text: &str) -> Value {
        presets::create(
            &self.pool,
            self.owner,
            &Uuid::now_v7().to_string(),
            presets::NewPreset {
                title: title.to_owned(),
                description: None,
                text: text.to_owned(),
            },
            presets::Actor::Owner,
            Uuid::now_v7(),
        )
        .await
        .map(|preset| serde_json::to_value(preset).unwrap())
        .unwrap()
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn agent_call_requires_live_execution_on_the_calling_machine(pool: PgPool) {
    let fx = Fx::new(pool).await;
    // Неизвестное исполнение.
    let unknown = WorkerData::agent_call(
        &fx.grpc,
        fx.request(
            "presets.list",
            json!({}),
            &Uuid::now_v7(),
            Some((Uuid::now_v7(), 1)),
            None,
        ),
    )
    .await;
    assert!(unknown.is_ok(), "неизвестное исполнение: {unknown:?}");
    assert_eq!(
        unknown.unwrap().into_inner().error_code,
        "execution_not_live"
    );
    // Устаревшее поколение.
    let stale = WorkerData::agent_call(
        &fx.grpc,
        fx.request(
            "presets.list",
            json!({}),
            &Uuid::now_v7(),
            Some((fx.execution.execution_id, fx.execution.generation + 1)),
            None,
        ),
    )
    .await;
    assert!(stale.is_ok(), "устаревшее поколение: {stale:?}");
    assert_eq!(stale.unwrap().into_inner().error_code, "execution_not_live");
    // Исполнение другой машины.
    let other = machines::add(&fx.pool, fx.owner, "чужая").await.unwrap();
    let other_conversation = testkit::seed_conversation(&fx.pool, fx.owner, other.machine_id).await;
    let other_execution = executions::start(&fx.pool, fx.owner, other_conversation, Uuid::now_v7())
        .await
        .unwrap();
    let foreign = WorkerData::agent_call(
        &fx.grpc,
        fx.request(
            "presets.list",
            json!({}),
            &Uuid::now_v7(),
            Some((other_execution.execution_id, other_execution.generation)),
            None,
        ),
    )
    .await;
    assert!(foreign.is_ok(), "исполнение другой машины: {foreign:?}");
    assert_eq!(
        foreign.unwrap().into_inner().error_code,
        "execution_not_live"
    );
    // Остановленное исполнение.
    executions::set_state(
        &fx.pool,
        fx.execution.execution_id,
        canterlot_hub::model::ExecutionState::Stopped,
        Some("start_cancelled"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let stopped = WorkerData::agent_call(
        &fx.grpc,
        fx.request("presets.list", json!({}), &Uuid::now_v7(), None, None),
    )
    .await;
    assert!(stopped.is_ok(), "остановленное исполнение: {stopped:?}");
    assert_eq!(
        stopped.unwrap().into_inner().error_code,
        "execution_not_live"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn list_returns_summaries_applied_and_pending_and_text_only_by_id(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let secret = format!("секретный-текст-пресета-{}", Uuid::now_v7());
    let created = fx.create_preset("Агентский список", &secret).await;
    let preset_id = created["id"].as_str().unwrap().to_owned();
    let response = fx.agent("presets.list", json!({}), &Uuid::now_v7()).await;
    assert_eq!(response.error_code, "");
    let body: Value = serde_json::from_str(&response.result_json).unwrap();
    let presets = body["presets"].as_array().unwrap();
    assert!(presets.len() >= 3, "встроенные и созданный: {body}");
    assert_eq!(presets[0]["id"], "clean");
    assert_eq!(presets[1]["id"], "owner");
    assert!(
        presets.iter().any(|preset| preset["id"] == preset_id),
        "{body}"
    );
    assert_eq!(body["applied"]["id"], "clean");
    assert!(body["pending"].is_null(), "{body}");
    assert!(body.get("preset").is_none(), "{body}");
    assert!(!response.result_json.contains(&secret), "текст не в списке");
    // Только по id агент читает текст.
    let response = fx
        .agent("presets.list", json!({"id": preset_id}), &Uuid::now_v7())
        .await;
    assert_eq!(response.error_code, "");
    let body: Value = serde_json::from_str(&response.result_json).unwrap();
    assert_eq!(body["preset"]["text"], secret);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn save_creates_agent_origin_preset_and_updates_with_base_revision(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let call_id = Uuid::now_v7();
    let response = fx
        .agent(
            "presets.save",
            json!({"title": "Агентский пресет", "text": "текст агента"}),
            &call_id,
        )
        .await;
    assert_eq!(response.error_code, "", "{}", response.error_message);
    let body: Value = serde_json::from_str(&response.result_json).unwrap();
    let id: Uuid = body["id"].as_str().unwrap().parse().unwrap();
    assert_eq!(body["revision"], 1);
    let mut conn = fx.pool.acquire().await.unwrap();
    let stored = presets::get(&mut conn, fx.owner, &presets::PresetRef::Custom(id))
        .await
        .unwrap();
    assert_eq!(stored.summary.created.by, "agent");
    assert_eq!(
        stored.summary.created.conversation_id,
        Some(fx.conversation)
    );
    // Правка с текущей базовой версией.
    let response = fx
        .agent(
            "presets.save",
            json!({"id": id.to_string(), "title": "Агентский пресет", "text": "текст агента v2", "base_revision": 1}),
            &Uuid::now_v7(),
        )
        .await;
    assert_eq!(response.error_code, "", "{}", response.error_message);
    let body: Value = serde_json::from_str(&response.result_json).unwrap();
    assert_eq!(body["revision"], 2);
    let stored = presets::get(&mut conn, fx.owner, &presets::PresetRef::Custom(id))
        .await
        .unwrap();
    assert_eq!(stored.summary.last_edit.as_ref().unwrap().by, "agent");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn save_refuses_builtins_stale_revision_and_missing_base_revision(pool: PgPool) {
    let fx = Fx::new(pool).await;
    for builtin in ["clean", "owner"] {
        let response = fx
            .agent(
                "presets.save",
                json!({"id": builtin, "title": "Чужое", "text": "текст", "base_revision": 0}),
                &Uuid::now_v7(),
            )
            .await;
        assert_eq!(response.error_code, "preset_builtin", "{builtin}");
    }
    let created = fx.create_preset("Агентский конфликт", "текст v1").await;
    let preset_id = created["id"].as_str().unwrap().to_owned();
    let revision = created["revision"].as_i64().unwrap();
    // Без base_revision правка запрещена.
    let response = fx
        .agent(
            "presets.save",
            json!({"id": preset_id, "title": "Агентский конфликт", "text": "текст v2"}),
            &Uuid::now_v7(),
        )
        .await;
    assert_eq!(response.error_code, "invalid_arguments");
    // Устаревшая базовая версия.
    let response = fx
        .agent(
            "presets.save",
            json!({"id": preset_id, "title": "Агентский конфликт", "text": "текст v2", "base_revision": revision + 5}),
            &Uuid::now_v7(),
        )
        .await;
    assert_eq!(response.error_code, "preset_revision_conflict");
    assert!(
        response.error_message.contains(&revision.to_string()),
        "текущая версия в сообщении: {}",
        response.error_message
    );
    // Неизвестный пресет.
    let response = fx
        .agent(
            "presets.save",
            json!({"id": Uuid::now_v7().to_string(), "title": "Нет", "text": "текст", "base_revision": 1}),
            &Uuid::now_v7(),
        )
        .await;
    assert_eq!(response.error_code, "preset_not_found");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn apply_sets_pending_on_own_conversation_only(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let created = fx
        .create_preset("Агентское применение", "текст применения")
        .await;
    let preset_id = created["id"].as_str().unwrap().to_owned();
    let response = fx
        .agent("presets.apply", json!({"id": preset_id}), &Uuid::now_v7())
        .await;
    assert_eq!(response.error_code, "", "{}", response.error_message);
    let body: Value = serde_json::from_str(&response.result_json).unwrap();
    assert_eq!(body["id"], preset_id);
    assert_eq!(body["applies"], "next_turn");
    let detail = conversations::detail(
        &mut fx.pool.acquire().await.unwrap(),
        fx.owner,
        fx.conversation,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        detail.settings.pending["instruction_profile"], preset_id,
        "подвешенная смена своей сессии"
    );
    // Вызов от имени другого исполнения меняет только его сессию.
    let other_conversation = testkit::seed_conversation(&fx.pool, fx.owner, fx.machine).await;
    sqlx::query("update conversations set model='synthetic-small',effort='high' where id=$1")
        .bind(other_conversation)
        .execute(&fx.pool)
        .await
        .unwrap();
    let other = executions::start(&fx.pool, fx.owner, other_conversation, Uuid::now_v7())
        .await
        .unwrap();
    let result = WorkerData::agent_call(
        &fx.grpc,
        fx.request(
            "presets.apply",
            json!({"id": "clean"}),
            &Uuid::now_v7(),
            Some((other.execution_id, other.generation)),
            None,
        ),
    )
    .await;
    assert!(result.is_ok(), "{result:?}");
    assert_eq!(result.unwrap().into_inner().error_code, "");
    let mut conn = fx.pool.acquire().await.unwrap();
    let other_detail = conversations::detail(&mut conn, fx.owner, other_conversation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        other_detail.settings.pending["instruction_profile"],
        "clean"
    );
    let own_detail = conversations::detail(&mut conn, fx.owner, fx.conversation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        own_detail.settings.pending["instruction_profile"], preset_id,
        "чужая сессия не тронута"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn same_call_id_is_idempotent(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let call_id = Uuid::now_v7();
    let args = json!({"title": "Идемпотентный", "text": "текст"});
    let first = fx.agent("presets.save", args.clone(), &call_id).await;
    assert_eq!(first.error_code, "", "{}", first.error_message);
    let second = fx.agent("presets.save", args, &call_id).await;
    assert_eq!(second.error_code, "", "{}", second.error_message);
    assert_eq!(first.result_json, second.result_json, "повтор вернул ответ");
    // Тот же call_id с другим телом — конфликт, а не вторая запись.
    let conflict = fx
        .agent(
            "presets.save",
            json!({"title": "Другой заголовок", "text": "текст"}),
            &call_id,
        )
        .await;
    assert_eq!(conflict.error_code, "idempotency_conflict");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn unknown_tool_and_bad_arguments_are_error_codes_not_status(pool: PgPool) {
    let fx = Fx::new(pool).await;
    let response = fx.agent("presets.delete", json!({}), &Uuid::now_v7()).await;
    assert_eq!(response.error_code, "unknown_tool");
    // Не объект.
    let response = fx.agent("presets.list", json!([]), &Uuid::now_v7()).await;
    assert_eq!(response.error_code, "invalid_arguments");
    // Неизвестное поле.
    let response = fx
        .agent("presets.list", json!({"unknown": "поле"}), &Uuid::now_v7())
        .await;
    assert_eq!(response.error_code, "invalid_arguments");
    // Неверный тип.
    let response = fx
        .agent(
            "presets.save",
            json!({"title": 42, "text": "текст"}),
            &Uuid::now_v7(),
        )
        .await;
    assert_eq!(response.error_code, "invalid_arguments");
    // Тело больше лимита.
    let big = "я".repeat(canterlot_proto::limits::MAX_AGENT_CALL_BYTES + 1);
    let oversized = AgentCallRequest {
        execution_id: fx.execution.execution_id.to_string(),
        generation: fx.execution.generation as u64,
        call_id: Uuid::now_v7().to_string(),
        tool: "presets.list".to_owned(),
        arguments_json: big,
    };
    let mut raw = Request::new(oversized);
    raw.metadata_mut().insert(
        "authorization",
        format!("Bearer {}", fx.token).parse().unwrap(),
    );
    let result = WorkerData::agent_call(&fx.grpc, raw).await;
    assert!(
        result.is_ok(),
        "переполнение — код, а не статус: {result:?}"
    );
    assert_eq!(result.unwrap().into_inner().error_code, "too_large");
}

use std::sync::{Mutex, OnceLock};

static BUFFER: OnceLock<Mutex<Vec<u8>>> = OnceLock::new();
#[derive(Clone)]
struct Capture;
impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        BUFFER
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .unwrap()
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        Capture
    }
}
fn capture_logs() {
    let _ = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(Capture)
        .try_init();
}

fn captured_logs() -> String {
    String::from_utf8(
        BUFFER
            .get()
            .expect("перехват журнала установлен")
            .lock()
            .unwrap()
            .clone(),
    )
    .unwrap()
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn agent_call_logs_contain_no_arguments(pool: PgPool) {
    capture_logs();
    let fx = Fx::new(pool).await;
    let marker = format!("СЕКРЕТ_АРГУМЕНТОВ_{}", Uuid::now_v7());
    let call_id = Uuid::now_v7();
    let response = fx
        .agent(
            "presets.save",
            json!({"title": "Журнал без текста", "text": marker}),
            &call_id,
        )
        .await;
    assert_eq!(response.error_code, "", "{}", response.error_message);
    let list = fx.agent("presets.list", json!({}), &Uuid::now_v7()).await;
    assert_eq!(list.error_code, "");
    let logs = captured_logs();
    assert!(!logs.contains(&marker), "аргументы в журнале: {logs}");
    assert!(
        logs.contains(&call_id.to_string()),
        "идентификатор вызова в журнале: {logs}"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn session_tool_stubs_answer_unknown_tool_after_the_liveness_check(pool: PgPool) {
    let fx = Fx::new(pool).await;
    for tool in canterlot_hub::session_tools::TOOLS {
        if matches!(*tool, "sessions.spawn" | "sessions.accept") {
            let response = fx.agent(tool, json!({}), &Uuid::now_v7()).await;
            assert_eq!(response.error_code, "unknown_tool", "{tool}");
            assert!(response.result_json.is_empty(), "{tool}");
        }
        // Не объект: проверка аргументов общая для всех инструментов.
        let response = fx.agent(tool, json!([]), &Uuid::now_v7()).await;
        assert_eq!(response.error_code, "invalid_arguments", "{tool}");
    }
    let stale = WorkerData::agent_call(
        &fx.grpc,
        fx.request(
            "sessions.list",
            json!({}),
            &Uuid::now_v7(),
            Some((Uuid::now_v7(), 1)),
            None,
        ),
    )
    .await
    .unwrap()
    .into_inner();
    assert_eq!(stale.error_code, "execution_not_live");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn tool_failure_logs_no_text(pool: PgPool) {
    capture_logs();
    let fx = Fx::new(pool).await;
    let marker = format!("СЕКРЕТ_ТЕКСТА_{}", Uuid::now_v7());
    let cases = [
        (
            json!({"session_id": Uuid::now_v7(), "text": marker}),
            "session_not_found",
        ),
        (
            json!({"session_id": marker, "text": marker}),
            "invalid_arguments",
        ),
        (
            json!({"session_id": fx.conversation, "text": marker}),
            "session_not_found",
        ),
        (
            json!({"session_id": fx.conversation, "text": marker.repeat(2000)}),
            "text_too_large",
        ),
    ];
    let mut calls = Vec::new();
    for (arguments, expected) in cases {
        let call_id = Uuid::now_v7();
        let response = fx.agent("sessions.send", arguments, &call_id).await;
        assert_eq!(response.error_code, expected);
        assert!(!response.error_message.contains(&marker));
        calls.push(call_id);
    }
    let logs = captured_logs();
    assert!(!logs.contains(&marker), "text in the log: {logs}");
    for call_id in calls {
        assert!(logs.contains(&call_id.to_string()));
    }
}

#[test]
fn session_tool_error_codes_are_unique_snake_case_with_messages() {
    use canterlot_hub::session_tools::ToolErrorCode;
    let mut seen = std::collections::BTreeSet::new();
    for code in ToolErrorCode::ALL {
        let wire = code.as_str();
        assert!(seen.insert(wire), "{wire} repeats");
        assert!(wire.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        assert!(!code.default_message().is_empty());
    }
    for expected in [
        "session_not_found",
        "read_only_session",
        "rate_limited",
        "chain_depth_exceeded",
        "spawn_depth_exceeded",
        "harness_unknown",
        "model_unknown",
        "effort_not_allowed",
        "workdir_not_allowed",
        "permission_mode_too_wide",
        "profile_unknown",
        "machine_unavailable",
        "capability_missing",
        "queue_full",
        "conversation_closed",
        "not_parent",
        "not_delegated",
        "text_too_large",
        "invalid_arguments",
    ] {
        assert!(seen.contains(expected), "{expected} missing");
    }
}
