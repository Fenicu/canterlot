//! Инструменты сессий для агента не пересекают домены памяти: разговор другого домена
//! неотличим от несуществующего (решение владельца (c)).

use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    config::SessionToolsConfig,
    session_tools::{
        self, Caller, ToolContext, ToolError, ToolErrorCode as Code, settle::SettleConfig,
    },
    testkit,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Fx {
    pool: PgPool,
    context: ToolContext,
    owner: Uuid,
    machine: Uuid,
    /// Разговор с доменом `work`, `personal` и без домена.
    work: Uuid,
    personal: Uuid,
    null: Uuid,
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let seed = testkit::seed(&pool).await;
        let context = ToolContext {
            memory: Default::default(),
            pool: pool.clone(),
            config: Arc::new(SessionToolsConfig::default()),
            limits: Default::default(),
            settle: SettleConfig {
                idle_quiet: Duration::from_secs(5),
                idle_background_max: Duration::from_secs(15 * 60),
                nudge_enabled: true,
            },
            alarms_enabled: true,
            nudge_enabled: true,
            routing_enabled: true,
            routing_stale: Duration::from_secs(1800),
            routing_queue_horizon: Duration::from_secs(6 * 3600),
            spawn_queue: Default::default(),
            guards: Default::default(),
            dispatcher: None,
            cloud: None,
            registry: canterlot_hub::session::Registry::new(Default::default()),
            shutdown: tokio_util::sync::CancellationToken::new(),
        };
        let f = Self {
            pool,
            context,
            owner: seed.owner_id,
            machine: seed.machine_id,
            work: seed.conversation_id,
            personal: Uuid::nil(),
            null: Uuid::nil(),
        };
        let personal = f.conversation("личный", Some("personal")).await;
        let null = f.conversation("без домена", None).await;
        sqlx::query("update conversations set memory_domain='work', title='рабочий' where id=$1")
            .bind(f.work)
            .execute(&f.pool)
            .await
            .unwrap();
        Self {
            personal,
            null,
            ..f
        }
    }

    async fn conversation(&self, title: &str, domain: Option<&str>) -> Uuid {
        let id = testkit::seed_conversation(&self.pool, self.owner, self.machine).await;
        sqlx::query("update conversations set title=$2, memory_domain=$3 where id=$1")
            .bind(id)
            .bind(title)
            .bind(domain)
            .execute(&self.pool)
            .await
            .unwrap();
        sqlx::query(
            "insert into executions(id,conversation_id,generation,state) values($1,$2,1,'stopped')",
        )
        .bind(Uuid::now_v7())
        .bind(id)
        .execute(&self.pool)
        .await
        .unwrap();
        id
    }

    async fn call(&self, from: Uuid, tool: &str, args: Value) -> Result<Value, ToolError> {
        let caller = Caller {
            owner_id: self.owner,
            conversation_id: from,
            execution_id: Uuid::now_v7(),
            machine_id: self.machine,
            call_id: Uuid::now_v7(),
        };
        session_tools::call(&self.context, &caller, tool, args).await
    }

    async fn say(&self, conversation: Uuid, seq: i64, text: &str) {
        sqlx::query(
            "insert into executions(id,conversation_id,generation,state)
             select $1,$2,1,'stopped'
             where not exists(select 1 from executions where conversation_id=$2)",
        )
        .bind(Uuid::now_v7())
        .bind(conversation)
        .execute(&self.pool)
        .await
        .unwrap();
        sqlx::query(
            "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_seq,
                                content,agent,model)
             select gen_random_uuid(),e.id,$1,'text','final',1,$2,$3,'main','small'
             from executions e where e.conversation_id=$1",
        )
        .bind(conversation)
        .bind(seq)
        .bind(text)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    async fn listed(&self, from: Uuid) -> Vec<Uuid> {
        self.call(from, "sessions.list", json!({})).await.unwrap()["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| serde_json::from_value(s["id"].clone()).unwrap())
            .collect()
    }

    /// Пары «вызывающий, цель» из разных доменов, в обе стороны.
    fn crossings(&self) -> [(Uuid, Uuid); 6] {
        [
            (self.work, self.personal),
            (self.personal, self.work),
            (self.work, self.null),
            (self.null, self.work),
            (self.personal, self.null),
            (self.null, self.personal),
        ]
    }
}

async fn not_found(f: &Fx, from: Uuid, tool: &str, args: Value) {
    let error = f.call(from, tool, args).await.unwrap_err();
    assert_eq!(error.code, Code::SessionNotFound, "{tool}: {error:?}");
}

#[sqlx::test(migrations = "./migrations")]
async fn cross_domain_inspect_is_not_found(pool: PgPool) {
    let f = Fx::new(pool).await;
    for (from, to) in f.crossings() {
        not_found(&f, from, "sessions.inspect", json!({"session_id": to})).await;
    }
    // Своего домена по-прежнему видно.
    let same = f.conversation("второй рабочий", Some("work")).await;
    f.call(f.work, "sessions.inspect", json!({"session_id": same}))
        .await
        .unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn cross_domain_history_search_returns_nothing_from_the_other_domain(pool: PgPool) {
    let f = Fx::new(pool).await;
    for (id, seq) in [(f.work, 1), (f.personal, 2), (f.null, 3)] {
        f.say(id, seq, "частый токен").await;
    }
    for (from, own) in [(f.work, f.work), (f.personal, f.personal), (f.null, f.null)] {
        let found = f
            .call(from, "history.search", json!({"query": "токен"}))
            .await
            .unwrap();
        let ids: Vec<Uuid> = found["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| serde_json::from_value(hit["session_id"].clone()).unwrap())
            .collect();
        assert_eq!(ids, vec![own], "поиск видит только свой домен");
        assert_eq!(found["total"], 1);
    }
    for (from, to) in f.crossings() {
        not_found(
            &f,
            from,
            "history.search",
            json!({"query": "токен", "session_id": to}),
        )
        .await;
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn cross_domain_send_subscribe_wait_spawn_target_adopt_alarm_are_not_found(pool: PgPool) {
    let f = Fx::new(pool).await;
    for (from, to) in f.crossings() {
        not_found(
            &f,
            from,
            "sessions.send",
            json!({"session_id": to, "text": "привет"}),
        )
        .await;
        not_found(&f, from, "sessions.subscribe", json!({"session_id": to})).await;
        not_found(&f, from, "sessions.wait", json!({"session_id": to})).await;
        not_found(&f, from, "plan.adopt", json!({"from_session_id": to})).await;
        not_found(
            &f,
            from,
            "alarms.set",
            json!({"when": {"kind": "session_state", "session_ids": [to]}, "key": "k"}),
        )
        .await;
        not_found(&f, from, "nudge.set", json!({"session_id": to})).await;
        not_found(&f, from, "sessions.accept", json!({"session_id": to})).await;
        not_found(&f, from, "sessions.interrupt", json!({"session_id": to})).await;
        not_found(&f, from, "sessions.stop", json!({"session_id": to})).await;
    }
    // Отказ не оставил следов: ни ввода, ни подписки, ни будильника.
    let traces: i64 = sqlx::query_scalar(
        "select (select count(*) from inputs where origin is not null)
              + (select count(*) from session_subscriptions)
              + (select count(*) from alarms)",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(traces, 0);
}

/// Скрытая цель отвечает на `nudge_set` с путём отчёта так же, как несуществующий ID:
/// путь не проверяется и воркер не опрашивается.
#[sqlx::test(migrations = "./migrations")]
async fn nudge_set_with_report_path_hides_out_of_domain_target(pool: PgPool) {
    let f = Fx::new(pool).await;
    let args = |target: Uuid| json!({"session_id": target, "report_file": "/h/report.md"});
    let missing = f
        .call(f.work, "nudge.set", args(Uuid::now_v7()))
        .await
        .unwrap_err();
    assert_eq!(missing.code, Code::SessionNotFound);
    for (from, to) in f.crossings() {
        let error = f.call(from, "nudge.set", args(to)).await.unwrap_err();
        assert_eq!(
            format!("{error:?}"),
            format!("{missing:?}"),
            "{from} -> {to}"
        );
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn same_domain_send_still_works(pool: PgPool) {
    let f = Fx::new(pool).await;
    let other = f.conversation("второй рабочий", Some("work")).await;
    f.call(
        f.work,
        "sessions.send",
        json!({"session_id": other, "text": "привет"}),
    )
    .await
    .unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn sessions_list_hides_other_domain_and_null_for_domained_callers(pool: PgPool) {
    let f = Fx::new(pool).await;
    let listed = f.listed(f.work).await;
    assert!(listed.contains(&f.work));
    assert!(!listed.contains(&f.personal));
    assert!(!listed.contains(&f.null));
    let listed = f.listed(f.personal).await;
    assert!(listed.contains(&f.personal));
    assert!(!listed.contains(&f.work));
    assert!(!listed.contains(&f.null));
}

#[sqlx::test(migrations = "./migrations")]
async fn null_caller_sees_only_itself(pool: PgPool) {
    let f = Fx::new(pool).await;
    let other_null = f.conversation("второй без домена", None).await;
    let listed = f.listed(f.null).await;
    assert_eq!(
        listed,
        vec![f.null],
        "разговор без домена видит только себя"
    );
    // Домен вызывающего с доменом не видит ни одного разговора без домена.
    for from in [f.work, f.personal] {
        let listed = f.listed(from).await;
        assert!(!listed.contains(&f.null) && !listed.contains(&other_null));
    }
    // Себя виден и по прямому вызову.
    f.call(f.null, "sessions.inspect", json!({"session_id": f.null}))
        .await
        .unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn two_null_sessions_cannot_reach_each_other(pool: PgPool) {
    let f = Fx::new(pool).await;
    let other = f.conversation("второй без домена", None).await;
    for (id, seq) in [(f.null, 1), (other, 2)] {
        f.say(id, seq, "частый токен").await;
    }
    for (from, to) in [(f.null, other), (other, f.null)] {
        not_found(&f, from, "sessions.inspect", json!({"session_id": to})).await;
        not_found(
            &f,
            from,
            "sessions.send",
            json!({"session_id": to, "text": "привет"}),
        )
        .await;
        not_found(&f, from, "sessions.subscribe", json!({"session_id": to})).await;
        not_found(
            &f,
            from,
            "history.search",
            json!({"query": "токен", "session_id": to}),
        )
        .await;
        let found = f
            .call(from, "history.search", json!({"query": "токен"}))
            .await
            .unwrap();
        let ids: Vec<Uuid> = found["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| serde_json::from_value(hit["session_id"].clone()).unwrap())
            .collect();
        assert_eq!(ids, vec![from], "поиск без домена находит только себя");
    }
}
