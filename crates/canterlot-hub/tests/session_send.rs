use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    activity::{self, Thresholds},
    config::{SchedulerConfig, SessionToolsConfig},
    conversations::{self, SubmitMessage},
    delivery::ContentBlock,
    dispatcher::Dispatcher,
    executions, history,
    model::ExecutionState,
    scheduler::Scheduler,
    session_tools::{
        self, Caller, ToolContext, ToolError, ToolErrorCode as Code, settle::SettleConfig,
    },
    testkit,
};
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

const QUIET: Duration = Duration::from_secs(5);

struct Fx {
    pool: PgPool,
    context: ToolContext,
    owner: Uuid,
    machine: Uuid,
    a: Uuid,
    b: Uuid,
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        Self::with(pool, SessionToolsConfig::default()).await
    }

    async fn with(pool: PgPool, config: SessionToolsConfig) -> Self {
        let seed = testkit::seed(&pool).await;
        let b = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
        sqlx::query("update machines set availability='available',capabilities=array['pilot_v1'],limits_ok=true where id=$1")
            .bind(seed.machine_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "update conversations set title=case when id=$1 then 'Сессия А' else 'Сессия Б' end,
               model='small' where id in ($1,$2)",
        )
        .bind(seed.conversation_id)
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
        Self {
            context: ToolContext {
                pool: pool.clone(),
                config: Arc::new(config),
                limits: Default::default(),
                settle: SettleConfig {
                    idle_quiet: QUIET,
                    idle_background_max: std::time::Duration::from_secs(15 * 60),
                    nudge_enabled: true,
                },
                alarms_enabled: true,
                nudge_enabled: true,
                routing_enabled: true,
                routing_stale: std::time::Duration::from_secs(1800),
                routing_queue_horizon: std::time::Duration::from_secs(6 * 3600),
                spawn_queue: Default::default(),
                guards: Default::default(),
                dispatcher: None,
                registry: canterlot_hub::session::Registry::new(Default::default()),
                shutdown: tokio_util::sync::CancellationToken::new(),
            },
            pool,
            owner: seed.owner_id,
            machine: seed.machine_id,
            a: seed.conversation_id,
            b,
        }
    }

    fn caller(&self, conversation: Uuid) -> Caller {
        Caller {
            owner_id: self.owner,
            conversation_id: conversation,
            execution_id: Uuid::now_v7(),
            machine_id: self.machine,
            call_id: Uuid::now_v7(),
        }
    }

    async fn call(&self, from: Uuid, tool: &str, args: Value) -> Result<Value, ToolError> {
        session_tools::call(&self.context, &self.caller(from), tool, args).await
    }

    async fn send(&self, from: Uuid, to: Uuid, text: &str) -> Result<Value, ToolError> {
        self.call(
            from,
            "sessions.send",
            json!({"session_id": to, "text": text}),
        )
        .await
    }

    async fn conversation(&self) -> Uuid {
        testkit::seed_conversation(&self.pool, self.owner, self.machine).await
    }

    /// Ввод принят агентом и ход закончен: так сессия видит его как свой последний ввод.
    async fn accept_all(&self, conversation: Uuid) {
        sqlx::query("update inputs set state='accepted' where conversation_id=$1")
            .bind(conversation)
            .execute(&self.pool)
            .await
            .unwrap();
    }

    async fn owner_input(&self, conversation: Uuid) {
        conversations::submit_message(
            &self.pool,
            SubmitMessage {
                owner_id: self.owner,
                conversation_id: conversation,
                key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text {
                    text: "от владельца".into(),
                }],
                attachment_ids: vec![],
                retry_of: None,
                received_at: Utc::now(),
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap();
    }

    async fn input(&self, id: &Value) -> (Value, Value, String) {
        let id: Uuid = serde_json::from_value(id.clone()).unwrap();
        sqlx::query_as::<_, (Value, Option<Value>, String)>(
            "select content,origin,state from inputs where id=$1",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await
        .map(|(content, origin, state)| (content, origin.unwrap_or(Value::Null), state))
        .unwrap()
    }

    async fn inputs(&self, conversation: Uuid) -> i64 {
        sqlx::query_scalar("select count(*) from inputs where conversation_id=$1")
            .bind(conversation)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn depths(&self, conversation: Uuid) -> Vec<i32> {
        sqlx::query_scalar(
            "select (origin->>'chain_depth')::int from inputs
             where conversation_id=$1 and origin is not null order by input_seq",
        )
        .bind(conversation)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn subscription(&self, subscriber: Uuid, target: Uuid) -> Option<Value> {
        sqlx::query_scalar(
            "select to_jsonb(s) from session_subscriptions s
             where subscriber_id=$1 and target_id=$2",
        )
        .bind(subscriber)
        .bind(target)
        .fetch_optional(&self.pool)
        .await
        .unwrap()
    }

    async fn event_jobs(&self, subscriber: Uuid) -> Vec<(String, Value)> {
        sqlx::query_as(
            "select state,payload from scheduled_jobs
             where kind='session_task' and conversation_id=$1 order by id",
        )
        .bind(subscriber)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn refresh(&self, conversation: Uuid, now: DateTime<Utc>) {
        let thresholds = Thresholds {
            stalled_generating: Duration::from_secs(300),
            stalled_tool: Duration::from_secs(1800),
            idle_quiet: QUIET,
            idle_background_max: std::time::Duration::from_secs(15 * 60),
        };
        activity::refresh(&self.pool, conversation, now, &thresholds, Uuid::now_v7())
            .await
            .unwrap();
    }

    async fn attachment(&self, conversation: Uuid, name: &str, mime: &str) -> Uuid {
        let id = Uuid::now_v7();
        sqlx::query(
            "insert into attachments (id,owner_id,conversation_id,name,mime,size_bytes,sha256,state,ready_at)
             values($1,$2,$3,$4,$5,4,$6,'ready',clock_timestamp())",
        )
        .bind(id)
        .bind(self.owner)
        .bind(conversation)
        .bind(name)
        .bind(mime)
        .bind("0".repeat(64))
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    fn scheduler(&self) -> Scheduler {
        Scheduler::new(
            self.pool.clone(),
            Dispatcher::channel().0,
            SchedulerConfig::default(),
        )
    }
}

#[test]
fn sender_debug_has_no_title() {
    let sender = session_tools::origin::Sender {
        id: Uuid::now_v7(),
        title: "секретное название".into(),
        harness: "synthetic".into(),
        model: "small".into(),
    };
    assert!(!format!("{sender:?}").contains("секретное"));
}

#[test]
fn a_title_stays_inside_the_service_line() {
    let cleaned = session_tools::origin::clean_title("а\"\n\nб\tв");
    assert_eq!(cleaned, "а'  б в");
    assert_eq!(
        session_tools::origin::clean_title(&"я".repeat(500))
            .chars()
            .count(),
        200
    );
}

fn code(result: Result<Value, ToolError>) -> Code {
    result.expect_err("the call must fail").code
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn send_marks_origin_and_prefix(pool: PgPool) {
    let f = Fx::new(pool).await;
    let sent = f.send(f.a, f.b, "собери отчёт").await.unwrap();
    assert_eq!(sent["deduplicated"], false);
    assert_eq!(sent["subscribed"], true);
    assert!(sent["hint"].as_str().unwrap().starts_with("Queued"));

    let (content, origin, state) = f.input(&sent["input_id"]).await;
    assert_eq!(state, "queued");
    assert_eq!(
        content[0]["text"],
        format!(
            "[canterlot] Message from session \"Сессия А\" (id {}, synthetic, model small). \
             This is another agent, not the owner.\n\nсобери отчёт",
            f.a
        )
    );
    assert_eq!(origin["kind"], "message");
    assert_eq!(origin["from"], json!(f.a));
    assert_eq!(origin["title"], "Сессия А");
    assert_eq!(origin["harness"], "synthetic");
    assert_eq!(origin["model"], "small");
    assert_eq!(origin["chain_depth"], 1);
    assert_eq!(origin["sha256"].as_str().unwrap().len(), 64);

    let queued: Value = sqlx::query_scalar(
        "select payload from events where conversation_id=$1 and kind='input_queued'",
    )
    .bind(f.b)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(queued["origin"], origin);

    let mut conn = f.pool.acquire().await.unwrap();
    sqlx::query("update inputs set feed_seq=created_seq where conversation_id=$1")
        .bind(f.b)
        .execute(&mut *conn)
        .await
        .unwrap();
    let page = history::page(&mut conn, f.owner, f.b, history::HistoryCursor::Tail, 10)
        .await
        .unwrap()
        .unwrap();
    let view = serde_json::to_value(&page.items[0]).unwrap();
    assert_eq!(view["origin"], origin);

    f.owner_input(f.a).await;
    let queued: Value = sqlx::query_scalar(
        "select payload from events where conversation_id=$1 and kind='input_queued'",
    )
    .bind(f.a)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(queued["origin"], Value::Null, "the owner has no origin");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn send_refuses_itself_strangers_and_bad_arguments(pool: PgPool) {
    let f = Fx::new(pool).await;
    assert_eq!(
        code(f.send(f.a, f.a, "сам себе").await),
        Code::SessionNotFound
    );
    assert_eq!(
        code(f.send(f.a, Uuid::now_v7(), "никому").await),
        Code::SessionNotFound
    );
    let stranger = testkit::seed(&f.pool).await;
    assert_eq!(
        code(f.send(f.a, stranger.conversation_id, "чужому").await),
        Code::SessionNotFound
    );
    assert_eq!(code(f.send(f.a, f.b, " \n ").await), Code::InvalidArguments);
    assert_eq!(
        code(f.send(f.a, f.b, "нуль\0внутри").await),
        Code::InvalidArguments
    );
    assert_eq!(
        code(f.send(f.a, f.b, &"я".repeat(16 * 1024 + 1)).await),
        Code::TextTooLarge
    );
    assert_eq!(
        code(
            f.call(
                f.a,
                "sessions.send",
                json!({"session_id": f.b, "text": "x", "lol": 1})
            )
            .await
        ),
        Code::InvalidArguments
    );
    sqlx::query("update conversations set archived_at=now() where id=$1")
        .bind(f.b)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        code(f.send(f.a, f.b, "в архив").await),
        Code::ConversationClosed
    );
    assert_eq!(f.inputs(f.b).await, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn send_queues_for_a_stopped_target_and_says_so(pool: PgPool) {
    let f = Fx::new(pool).await;
    let sent = f.send(f.a, f.b, "проснись").await.unwrap();
    assert_eq!(sent["target_stopped"], true);
    assert!(sent["hint"].as_str().unwrap().contains("stopped"));
    assert_eq!(f.input(&sent["input_id"]).await.2, "queued");

    let live = executions::start(&f.pool, f.owner, f.b, Uuid::now_v7())
        .await
        .unwrap();
    executions::set_state(
        &f.pool,
        live.execution_id,
        ExecutionState::Starting,
        None,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let sent = f.send(f.a, f.b, "ещё").await.unwrap();
    assert_eq!(sent["target_stopped"], false);
    assert_eq!(
        sent["hint"],
        "Queued. You are subscribed: the result arrives in your queue as a [canterlot] message. \
         Do not poll; end your turn if you have nothing else to do."
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn send_dedups_within_window(pool: PgPool) {
    let f = Fx::new(pool).await;
    let first = f.send(f.a, f.b, "одно и то же").await.unwrap();
    let again = f.send(f.a, f.b, "одно и то же").await.unwrap();
    assert_eq!(again["input_id"], first["input_id"]);
    assert_eq!(again["deduplicated"], true);
    assert_eq!(f.inputs(f.b).await, 1);

    let other = f.send(f.a, f.b, "другое").await.unwrap();
    assert_eq!(other["deduplicated"], false);
    assert_eq!(f.inputs(f.b).await, 2);

    let third = f.conversation().await;
    let elsewhere = f.send(f.a, third, "одно и то же").await.unwrap();
    assert_eq!(elsewhere["deduplicated"], false, "another target");

    let from_b = f.send(third, f.b, "одно и то же").await.unwrap();
    assert_eq!(from_b["deduplicated"], false, "another sender");

    // Вне окна тот же текст снова новое сообщение.
    sqlx::query("update inputs set received_at=received_at-interval '11 minutes' where id=$1")
        .bind(serde_json::from_value::<Uuid>(first["input_id"].clone()).unwrap())
        .execute(&f.pool)
        .await
        .unwrap();
    let late = f.send(f.a, f.b, "одно и то же").await.unwrap();
    assert_eq!(late["deduplicated"], false);
    assert_ne!(late["input_id"], first["input_id"]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dedup_ends_when_the_first_input_is_finished(pool: PgPool) {
    let f = Fx::new(pool).await;
    let first = f.send(f.a, f.b, "повтор").await.unwrap();
    let input: Uuid = serde_json::from_value(first["input_id"].clone()).unwrap();
    for (state, deduplicated) in [
        ("dispatched", true),
        ("on_machine", true),
        ("transmitting", true),
        ("cancelled", false),
    ] {
        sqlx::query("update inputs set state=$2 where id=$1")
            .bind(input)
            .bind(state)
            .execute(&f.pool)
            .await
            .unwrap();
        let again = f.send(f.a, f.b, "повтор").await.unwrap();
        assert_eq!(again["deduplicated"], deduplicated, "{state}");
        if !deduplicated {
            assert_ne!(again["input_id"], first["input_id"]);
        }
        sqlx::query("delete from inputs where conversation_id=$1 and id<>$2")
            .bind(f.b)
            .bind(input)
            .execute(&f.pool)
            .await
            .unwrap();
    }

    // Принятый ввод не закончен, пока занимает слот хода.
    sqlx::query("update inputs set state='accepted' where id=$1")
        .bind(input)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2 where conversation_id=$1",
    )
    .bind(f.b)
    .bind(input)
    .execute(&f.pool)
    .await
    .unwrap();
    let again = f.send(f.a, f.b, "повтор").await.unwrap();
    assert_eq!(again["deduplicated"], true, "the turn is still running");
    sqlx::query(
        "update queue_state set run_slot='free',run_slot_input_id=null where conversation_id=$1",
    )
    .bind(f.b)
    .execute(&f.pool)
    .await
    .unwrap();
    let again = f.send(f.a, f.b, "повтор").await.unwrap();
    assert_eq!(again["deduplicated"], false, "the turn is over");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn dedup_does_not_spend_the_rate_limit_and_rearms_with_the_input_still_queued(pool: PgPool) {
    let f = Fx::with(
        pool,
        SessionToolsConfig {
            pair_sends: 1,
            ..Default::default()
        },
    )
    .await;
    let now = Utc::now();
    f.refresh(f.b, now - TimeDelta::hours(1)).await;
    let first = f.send(f.a, f.b, "текст").await.unwrap();
    sqlx::query("update session_subscriptions set closed_at=now() where subscriber_id=$1")
        .bind(f.a)
        .execute(&f.pool)
        .await
        .unwrap();

    let again = f.send(f.a, f.b, "текст").await.unwrap();
    assert_eq!(again["input_id"], first["input_id"]);
    assert_eq!(again["subscribed"], true);
    let row = f.subscription(f.a, f.b).await.unwrap();
    assert_eq!(row["closed_at"], Value::Null, "re-armed");

    // Ввод ещё в очереди: цель не оседает, повторный взвод не срабатывает сразу.
    f.refresh(f.b, now).await;
    assert!(f.event_jobs(f.a).await.is_empty());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_finished_first_input_gets_a_new_input_before_the_rearm(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    let first = f.send(f.a, f.b, "текст").await.unwrap();
    f.accept_all(f.b).await;
    f.refresh(f.b, now - TimeDelta::hours(1)).await;
    f.refresh(f.b, now).await;
    assert_eq!(f.event_jobs(f.a).await.len(), 1, "the first answer fired");

    let again = f.send(f.a, f.b, "текст").await.unwrap();
    assert_eq!(again["deduplicated"], false);
    assert_ne!(again["input_id"], first["input_id"]);
    f.refresh(f.b, now + TimeDelta::seconds(1)).await;
    assert_eq!(
        f.event_jobs(f.a).await.len(),
        1,
        "the new input is queued in the transaction of the re-arm"
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn transport_retry_of_a_finished_call_does_not_rearm(pool: PgPool) {
    let f = Fx::new(pool).await;
    let caller = f.caller(f.a);
    let args = json!({"session_id": f.b, "text": "один раз"});
    let now = Utc::now();
    let first = session_tools::call(&f.context, &caller, "sessions.send", args.clone())
        .await
        .unwrap();
    f.accept_all(f.b).await;
    f.refresh(f.b, now - TimeDelta::hours(1)).await;
    f.refresh(f.b, now).await;
    assert_eq!(f.event_jobs(f.a).await.len(), 1);
    assert!(
        f.subscription(f.a, f.b).await.unwrap()["closed_at"].is_string(),
        "idle closed it"
    );

    let retry = session_tools::call(&f.context, &caller, "sessions.send", args)
        .await
        .unwrap();
    assert_eq!(retry["input_id"], first["input_id"]);
    assert_eq!(retry["deduplicated"], false);
    assert_eq!(f.inputs(f.b).await, 1);
    assert!(f.subscription(f.a, f.b).await.unwrap()["closed_at"].is_string());
    f.refresh(f.b, now + TimeDelta::seconds(30)).await;
    assert_eq!(f.event_jobs(f.a).await.len(), 1, "no second event");

    let changed = session_tools::call(
        &f.context,
        &caller,
        "sessions.send",
        json!({"session_id": f.b, "text": "другой текст"}),
    )
    .await;
    assert_eq!(code(changed), Code::InvalidArguments);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn send_subscribes_by_default_and_opts_out(pool: PgPool) {
    let f = Fx::new(pool).await;
    let third = f.conversation().await;
    let quiet = f
        .call(
            f.a,
            "sessions.send",
            json!({"session_id": third, "text": "без подписки", "subscribe": false}),
        )
        .await
        .unwrap();
    assert_eq!(quiet["subscribed"], false);
    assert!(f.subscription(f.a, third).await.is_none());

    let sent = f.send(f.a, f.b, "с подпиской").await.unwrap();
    assert_eq!(sent["subscribed"], true);
    let row = f.subscription(f.a, f.b).await.unwrap();
    assert_eq!(row["closed_at"], Value::Null);
    assert_eq!(row["last_fingerprint"], Value::Null);
    assert_eq!(row["chain_depth"], 0, "the sender's own depth");

    // Отказ не отменяет уже открытую подписку, и ответ об этом не врёт.
    let still = f
        .call(
            f.a,
            "sessions.send",
            json!({"session_id": f.b, "text": "ещё", "subscribe": false}),
        )
        .await
        .unwrap();
    assert_eq!(still["subscribed"], true);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn pair_rate_limit(pool: PgPool) {
    let f = Fx::with(
        pool,
        SessionToolsConfig {
            pair_sends: 3,
            ..Default::default()
        },
    )
    .await;
    for i in 0..3 {
        f.send(f.a, f.b, &format!("сообщение {i}")).await.unwrap();
    }
    assert_eq!(code(f.send(f.a, f.b, "лишнее").await), Code::RateLimited);
    assert_eq!(f.inputs(f.b).await, 3);

    let third = f.conversation().await;
    f.send(f.a, third, "другая пара").await.unwrap();

    sqlx::query(
        "update inputs set received_at=received_at-interval '11 minutes' where conversation_id=$1",
    )
    .bind(f.b)
    .execute(&f.pool)
    .await
    .unwrap();
    f.send(f.a, f.b, "окно прошло").await.unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn sender_rate_limit(pool: PgPool) {
    let f = Fx::with(
        pool,
        SessionToolsConfig {
            sender_sends: 3,
            ..Default::default()
        },
    )
    .await;
    for i in 0..3 {
        let target = f.conversation().await;
        f.send(f.a, target, &format!("сообщение {i}"))
            .await
            .unwrap();
    }
    assert_eq!(code(f.send(f.a, f.b, "лишнее").await), Code::RateLimited);
    f.send(f.b, f.a, "у другого отправителя свой счёт")
        .await
        .unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn events_do_not_count_toward_the_rate_limit(pool: PgPool) {
    let f = Fx::with(
        pool,
        SessionToolsConfig {
            pair_sends: 1,
            ..Default::default()
        },
    )
    .await;
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,kind,payload,delivery_state)
         select id,$2,$1,'deliver_input','{}','completed'
           from (select gen_random_uuid() id from generate_series(1,5)) g",
    )
    .bind(f.b)
    .bind(f.machine)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into inputs(id,conversation_id,owner_id,input_seq,idempotency_key,content,state,command_id,received_at,origin)
         select gen_random_uuid(),$1,$2,row_number() over (),'k'||c.id,'[]','accepted',c.id,now(),
                jsonb_build_object('kind','event','from',$3::text,'chain_depth',0)
           from commands c where c.conversation_id=$1",
    )
    .bind(f.b)
    .bind(f.owner)
    .bind(f.a)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query("update queue_state set next_input_seq=100 where conversation_id=$1")
        .bind(f.b)
        .execute(&f.pool)
        .await
        .unwrap();
    f.send(f.a, f.b, "первое настоящее").await.unwrap();
    assert_eq!(code(f.send(f.a, f.b, "второе").await), Code::RateLimited);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn chain_depth_stops_ping_pong(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (mut from, mut to) = (f.a, f.b);
    for depth in 1..=8 {
        let sent = f.send(from, to, &format!("шаг {depth}")).await.unwrap();
        assert_eq!(f.input(&sent["input_id"]).await.1["chain_depth"], depth);
        f.accept_all(to).await;
        (from, to) = (to, from);
    }
    assert_eq!(
        code(f.send(from, to, "девятый").await),
        Code::ChainDepthExceeded
    );
    assert_eq!(f.inputs(f.a).await + f.inputs(f.b).await, 8);
    assert_eq!(f.depths(f.a).await, vec![2, 4, 6, 8]);
    assert_eq!(f.depths(f.b).await, vec![1, 3, 5, 7]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn chain_depth_uses_the_input_of_the_running_turn(pool: PgPool) {
    let f = Fx::new(pool).await;
    let sent = f.send(f.a, f.b, "первое").await.unwrap();
    let input: Uuid = serde_json::from_value(sent["input_id"].clone()).unwrap();
    // Ход уже идёт, а квитанция о приёме ещё не дошла: вызов инструмента её обгоняет.
    sqlx::query(
        "update queue_state set run_slot='running',run_slot_input_id=$2 where conversation_id=$1",
    )
    .bind(f.b)
    .bind(input)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query("update inputs set state='transmitting' where id=$1")
        .bind(input)
        .execute(&f.pool)
        .await
        .unwrap();
    let back = f.send(f.b, f.a, "ответ").await.unwrap();
    assert_eq!(f.input(&back["input_id"]).await.1["chain_depth"], 2);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn owner_input_resets_chain(pool: PgPool) {
    let f = Fx::new(pool).await;
    let (mut from, mut to) = (f.a, f.b);
    for _ in 0..8 {
        f.send(from, to, &Uuid::now_v7().to_string()).await.unwrap();
        f.accept_all(to).await;
        (from, to) = (to, from);
    }
    assert_eq!(
        code(f.send(from, to, "глубоко").await),
        Code::ChainDepthExceeded
    );
    f.owner_input(from).await;
    f.accept_all(from).await;
    let sent = f.send(from, to, "после владельца").await.unwrap();
    assert_eq!(f.input(&sent["input_id"]).await.1["chain_depth"], 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn subscription_event_does_not_grow_chain(pool: PgPool) {
    let f = Fx::with(
        pool,
        SessionToolsConfig {
            pair_sends: 100,
            sender_sends: 100,
            ..Default::default()
        },
    )
    .await;
    f.refresh(f.b, Utc::now() - TimeDelta::hours(2)).await;
    f.owner_input(f.a).await;
    f.accept_all(f.a).await;
    let mut now = Utc::now() + TimeDelta::minutes(1);
    let scheduler = f.scheduler();
    // Оркестратор ведёт работу последовательно: сообщение, событие об итоге, следующее сообщение.
    for step in 0..25 {
        let sent = f.send(f.a, f.b, &format!("шаг {step}")).await.unwrap();
        assert_eq!(f.input(&sent["input_id"]).await.1["chain_depth"], 1);
        let row = f.subscription(f.a, f.b).await.unwrap();
        assert_eq!(row["chain_depth"], 0, "stored at subscribe time");
        f.accept_all(f.b).await;
        now += TimeDelta::minutes(1);
        f.refresh(f.b, now).await;
        assert_eq!(scheduler.tick(now).await.unwrap().fired, 1, "step {step}");
        f.accept_all(f.a).await;
    }
    let depths = f.depths(f.a).await;
    assert_eq!(depths.len(), 25);
    assert!(depths.iter().all(|depth| *depth == 0), "{depths:?}");
    assert_eq!(f.inputs(f.b).await, 25);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn read_only_sender_cannot_write_by_proxy(pool: PgPool) {
    let f = Fx::new(pool).await;
    let set = |id: Uuid, mode: &'static str, pending: Value| {
        let pool = f.pool.clone();
        async move {
            sqlx::query(
                "update conversations set permission_mode=$2,pending_settings=$3 where id=$1",
            )
            .bind(id)
            .bind(mode)
            .bind(pending)
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    set(f.a, "read_only", json!({})).await;
    set(f.b, "ask", json!({})).await;
    assert_eq!(
        f.send(f.a, f.b, "запиши").await.unwrap_err().code,
        Code::ReadOnlySession
    );
    assert_eq!(f.inputs(f.b).await, 0);

    set(f.b, "read_only", json!({})).await;
    f.send(f.a, f.b, "почитай").await.unwrap();

    // Ожидающая смена цели на более широкий режим считается: следующий ход пойдёт уже в нём.
    set(f.b, "read_only", json!({"permission_mode": "bypass"})).await;
    assert_eq!(
        f.send(f.a, f.b, "запиши потом").await.unwrap_err().code,
        Code::ReadOnlySession
    );

    // И наоборот: отправитель, который сам сужается до read_only, уже не пишет чужими руками.
    set(f.a, "ask", json!({"permission_mode": "read_only"})).await;
    set(f.b, "ask", json!({})).await;
    assert_eq!(
        f.send(f.a, f.b, "успею").await.unwrap_err().code,
        Code::ReadOnlySession
    );

    set(f.a, "ask", json!({})).await;
    f.send(f.a, f.b, "можно").await.unwrap();
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn subscribe_tool_reports_the_state_and_unsubscribe_cancels_the_pending_event(pool: PgPool) {
    let f = Fx::new(pool).await;
    let now = Utc::now();
    f.refresh(f.b, now - TimeDelta::hours(1)).await;
    let answer = f
        .call(f.a, "sessions.subscribe", json!({"session_id": f.b}))
        .await
        .unwrap();
    assert_eq!(answer["subscribed"], true);
    assert_eq!(answer["current_state"], "idle");
    assert!(answer["hint"].is_string());
    let row = f.subscription(f.a, f.b).await.unwrap();
    assert!(
        row["last_fingerprint"]
            .as_str()
            .unwrap()
            .starts_with("idle|")
    );
    f.refresh(f.b, now).await;
    assert!(f.event_jobs(f.a).await.is_empty(), "already answered");

    let gone = f
        .call(f.a, "sessions.unsubscribe", json!({"session_id": f.b}))
        .await
        .unwrap();
    assert_eq!(gone, json!({"subscribed": false}));
    let row = f.subscription(f.a, f.b).await.unwrap();
    assert_eq!(row["closed_reason"], "unsubscribed");

    let again = f
        .call(f.a, "sessions.unsubscribe", json!({"session_id": f.b}))
        .await
        .unwrap();
    assert_eq!(again, json!({"subscribed": false}), "idempotent");
    let row = f.subscription(f.a, f.b).await.unwrap();
    assert_eq!(row["closed_reason"], "unsubscribed");

    // Событие уже в очереди заданий: отписка забирает и его, чужие задания не трогает.
    let third = f.conversation().await;
    f.send(f.a, f.b, "работа").await.unwrap();
    f.send(f.a, third, "работа").await.unwrap();
    f.accept_all(f.b).await;
    f.accept_all(third).await;
    f.refresh(third, now - TimeDelta::hours(1)).await;
    f.refresh(f.b, now + TimeDelta::minutes(1)).await;
    f.refresh(third, now + TimeDelta::minutes(1)).await;
    assert_eq!(f.event_jobs(f.a).await.len(), 2);
    f.call(f.a, "sessions.unsubscribe", json!({"session_id": f.b}))
        .await
        .unwrap();
    let jobs = f.event_jobs(f.a).await;
    let states: Vec<_> = jobs
        .iter()
        .map(|(state, payload)| {
            (
                state.as_str(),
                payload["target_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        states,
        vec![
            ("cancelled", f.b.to_string()),
            ("pending", third.to_string())
        ]
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn subscribe_refuses_strangers_and_itself(pool: PgPool) {
    let f = Fx::new(pool).await;
    for tool in ["sessions.subscribe", "sessions.unsubscribe"] {
        assert_eq!(
            code(f.call(f.a, tool, json!({"session_id": f.a})).await),
            Code::SessionNotFound
        );
        assert_eq!(
            code(
                f.call(f.a, tool, json!({"session_id": Uuid::now_v7()}))
                    .await
            ),
            Code::SessionNotFound
        );
        assert_eq!(
            code(f.call(f.a, tool, json!({"session_id": "не uuid"})).await),
            Code::InvalidArguments
        );
    }
    assert!(f.subscription(f.a, f.a).await.is_none());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_full_target_queue_is_a_tool_error(pool: PgPool) {
    let f = Fx::with(
        pool,
        SessionToolsConfig {
            pair_sends: 1000,
            sender_sends: 1000,
            ..Default::default()
        },
    )
    .await;
    for i in 0..100 {
        f.send(f.a, f.b, &format!("сообщение {i}")).await.unwrap();
    }
    assert_eq!(code(f.send(f.a, f.b, "лишнее").await), Code::QueueFull);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn parallel_sends_cannot_outrun_the_limit_or_the_dedup(pool: PgPool) {
    let f = Fx::with(
        pool,
        SessionToolsConfig {
            pair_sends: 3,
            ..Default::default()
        },
    )
    .await;
    let distinct: Vec<_> = (0..8).map(|i| format!("сообщение {i}")).collect();
    let results =
        futures_util::future::join_all(distinct.iter().map(|text| f.send(f.a, f.b, text))).await;
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 3);
    assert_eq!(f.inputs(f.b).await, 3);

    // Разные цели не делят очередь: счёт отправителя держит только его собственная блокировка.
    let mut targets = Vec::new();
    for _ in 0..8 {
        targets.push(f.conversation().await);
    }
    sqlx::query("delete from inputs where conversation_id=$1")
        .bind(f.b)
        .execute(&f.pool)
        .await
        .unwrap();
    let wide = Fx {
        context: ToolContext {
            config: Arc::new(SessionToolsConfig {
                sender_sends: 3,
                ..Default::default()
            }),
            ..f.context.clone()
        },
        pool: f.pool.clone(),
        ..f
    };
    let results = futures_util::future::join_all(
        targets
            .iter()
            .map(|target| wide.send(wide.a, *target, "всем")),
    )
    .await;
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 3);
    let f = wide;

    let third = f.conversation().await;
    let same = futures_util::future::join_all((0..8).map(|_| f.send(f.b, third, "одно"))).await;
    assert!(same.iter().all(Result::is_ok));
    assert_eq!(f.inputs(third).await, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn attachments_of_the_callers_conversation_are_forwarded(pool: PgPool) {
    let f = Fx::new(pool).await;
    let file = f.attachment(f.a, "report.pdf", "application/pdf").await;
    let answer = f
        .call(
            f.a,
            "sessions.send",
            json!({"session_id": f.b, "text": "вот файл", "attachment_ids": [file]}),
        )
        .await
        .unwrap();
    let (content, _, _) = f.input(&answer["input_id"]).await;
    let blocks = content.as_array().unwrap();
    assert_eq!(blocks[1]["type"], "attachment");
    assert_eq!(blocks[1]["attachment_id"], file.to_string());
    assert_eq!(blocks[1]["name"], "report.pdf");
    let linked: i64 = sqlx::query_scalar(
        "select count(*) from input_attachments where attachment_id=$1
           and input_id=(select id from inputs where conversation_id=$2 and origin is not null)",
    )
    .bind(file)
    .bind(f.b)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(linked, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn foreign_attachment_is_refused(pool: PgPool) {
    let f = Fx::new(pool).await;
    let other = f.conversation().await;
    let file = f.attachment(other, "secret.pdf", "application/pdf").await;
    let error = f
        .call(
            f.a,
            "sessions.send",
            json!({"session_id": f.b, "text": "чужой файл", "attachment_ids": [file]}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::AttachmentNotAvailable);
    assert_eq!(f.inputs(f.b).await, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn image_attachment_is_refused_when_the_target_cannot_take_images(pool: PgPool) {
    let f = Fx::new(pool).await;
    let file = f.attachment(f.a, "shot.png", "image/png").await;
    let error = f
        .call(
            f.a,
            "sessions.send",
            json!({"session_id": f.b, "text": "картинка", "attachment_ids": [file]}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::AttachmentNotAvailable);
    assert!(error.message.contains("does not accept"));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_old_worker_without_pilot_refuses_attachments_cleanly(pool: PgPool) {
    let f = Fx::new(pool).await;
    let file = f.attachment(f.a, "old.pdf", "application/pdf").await;
    sqlx::query("update machines set capabilities=array[]::text[] where id=$1")
        .bind(f.machine)
        .execute(&f.pool)
        .await
        .unwrap();
    let error = f
        .call(
            f.a,
            "sessions.send",
            json!({"session_id": f.b, "text": "файл", "attachment_ids": [file]}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, Code::CapabilityMissing);
    assert_eq!(f.inputs(f.b).await, 0);
}
