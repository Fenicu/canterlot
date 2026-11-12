use crate::support::task9 as support;

use std::collections::{BTreeMap, BTreeSet};

use axum::http::Method;
use canterlot_hub::{
    delivery::{ContentBlock, SubmitInput, submit_input},
    events::{self, NewEvent, Scope},
    executions,
    ingest::{Ingest, IngestOutcome, JournalKind},
    machines,
    model::ExecutionState,
    session_tools::{self, Caller, ToolContext, settle::SettleConfig},
    testkit::{self, TestResponse},
};
use canterlot_proto::worker::v1::*;
use serde_json::{Value, json};
use sqlx::PgPool;
use support::*;
use uuid::Uuid;

/// Разговор со строками во всех таблицах, которые на него ссылаются.
struct Seeded {
    conversation: Uuid,
    execution: Uuid,
    journal: Uuid,
    command: Uuid,
    /// Команда ввода на машине: её квитанция может прийти после удаления.
    stored_command: Uuid,
    attachments: Vec<Uuid>,
    media: Uuid,
    /// Всё, по чему строку можно связать с разговором.
    needles: Vec<String>,
}

async fn exec(pool: &PgPool, sql: &'static str, ids: &[Uuid]) {
    let mut query = sqlx::query(sql);
    for id in ids {
        query = query.bind(*id);
    }
    query.execute(pool).await.unwrap();
}

async fn seed_everything(f: &Fixture, conversation: Uuid, machine: Uuid, group: Uuid) -> Seeded {
    let (pool, owner) = (&f.pool, f.owner);
    let (execution, journal, command, stored_command) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let (cancel, input, retry, block, child, request, job, media, compaction) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    exec(
        pool,
        "insert into conversation_derived(conversation_id) values($1) on conflict do nothing",
        &[conversation],
    )
    .await;
    exec(
        pool,
        "insert into executions(id,conversation_id,generation,state,outcome,native_conversation_id)
         values($1,$2,1,'stopped','user_stop','native-'||$2::text)",
        &[execution, conversation],
    )
    .await;
    exec(
        pool,
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,1,'deliver_input','{}','completed')",
        &[command, machine, conversation, execution],
    )
    .await;
    exec(
        pool,
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,1,'deliver_input','{}','stored_on_machine')",
        &[stored_command, machine, conversation, execution],
    )
    .await;
    exec(
        pool,
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state,target_command_id)
         values($1,$2,$3,$4,1,'cancel_input','{}','completed',$5)",
        &[cancel, machine, conversation, execution, command],
    )
    .await;
    exec(
        pool,
        "insert into inputs(id,conversation_id,owner_id,input_seq,idempotency_key,content,state,command_id,received_at)
         values($1,$2,$3,1,'seed-input','[]','accepted',$4,now())",
        &[input, conversation, owner, command],
    )
    .await;
    exec(
        pool,
        "insert into inputs(id,conversation_id,owner_id,input_seq,idempotency_key,content,state,command_id,received_at,retry_of)
         values($1,$2,$3,2,'seed-retry','[]','on_machine',$4,now(),$5)",
        &[retry, conversation, owner, stored_command, input],
    )
    .await;
    let linked = f
        .ready_in(conversation, "a.txt", "text/plain", &content(64, 1))
        .await;
    let scheduled = f
        .ready_in(conversation, "b.txt", "text/plain", &content(64, 2))
        .await;
    exec(
        pool,
        "insert into input_attachments(input_id,attachment_id,position) values($1,$2,0)",
        &[input, linked],
    )
    .await;
    exec(
        pool,
        "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,input_command_id)
         values($1,$2,$3,'text','final',1,$4)",
        &[block, execution, conversation, command],
    )
    .await;
    exec(
        pool,
        "insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,input_command_id,parent_block_id)
         values($1,$2,$3,'text','final',1,$4,$5)",
        &[child, execution, conversation, command, block],
    )
    .await;
    exec(
        pool,
        "insert into block_chunks(block_id,seq,text) values($1,1,'chunk')",
        &[block],
    )
    .await;
    exec(
        pool,
        "insert into bookmarks(id,owner_id,conversation_id,target_kind,target_id)
         values($1,$2,$3,'block',$4)",
        &[Uuid::now_v7(), owner, conversation, block],
    )
    .await;
    exec(
        pool,
        "insert into block_media(id,execution_id,conversation_id,block_id,mime,size_bytes,sha256)
         values($1,$2,$3,$4,'image/png',1,repeat('a',64))",
        &[media, execution, conversation, block],
    )
    .await;
    exec(
        pool,
        "insert into block_files(id,execution_id,conversation_id,block_id,attachment_id,name,mime,size_bytes,sha256,title,note)
         values($1,$2,$3,$4,$5,'a.txt','text/plain',1,repeat('a',64),'','')",
        &[Uuid::now_v7(), execution, conversation, block, linked],
    )
    .await;
    let path = f.blob_path(media);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"x").unwrap();
    exec(
        pool,
        "insert into requests(id,conversation_id,execution_id,generation,kind,native_request_id,state,request_payload)
         values($1,$2,$3,1,'permission','native-request','closed_without_decision','{}')",
        &[request, conversation, execution],
    )
    .await;
    exec(
        pool,
        "insert into runs(input_command_id,execution_id,conversation_id) values($1,$2,$3)",
        &[command, execution, conversation],
    )
    .await;
    exec(
        pool,
        "insert into run_model_usage(input_command_id,model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens)
         values($1,'opus','main',1,1,0,0)",
        &[command],
    )
    .await;
    exec(
        pool,
        "insert into conversation_usage_rollup(parent_id,source_id,turns,models)
         values($1,$2,1,'[{\"model\":\"opus\",\"input_tokens\":1,\"output_tokens\":1,\"cache_read_tokens\":0,\"cache_write_tokens\":0}]')",
        &[conversation, execution],
    )
    .await;
    exec(
        pool,
        "insert into agent_todos(conversation_id,generation,source_revision,revision,items,omitted)
         values($1,1,1,1,'[]',0)",
        &[conversation],
    )
    .await;
    exec(
        pool,
        "insert into background_tasks(conversation_id,task_id,execution_id,generation,source_revision,kind,description,state,started_at)
         values($1,'a1',$1,1,1,'agent','','running',now())",
        &[conversation],
    )
    .await;
    exec(
        pool,
        "insert into prompt_suggestions(conversation_id,execution_id,generation,text)
         values($1,$2,1,'next')",
        &[conversation, execution],
    )
    .await;
    exec(
        pool,
        "insert into compactions(id,conversation_id,execution_id,trigger,pre_tokens,ok,created_seq,finished_at)
         values($1,$2,$3,'auto',1,true,1,now())",
        &[compaction, conversation, execution],
    )
    .await;
    exec(
        pool,
        "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
         values($1,'message',$2,$3,now()+interval '1 day','{}')",
        &[job, owner, conversation],
    )
    .await;
    exec(
        pool,
        "insert into alarms(id,owner_id,conversation_id,created_by,kind,fire_at,call_id)
         values($1,$2,$3,'agent','at',now()+interval '1 hour',$4)",
        &[Uuid::now_v7(), owner, conversation, Uuid::now_v7()],
    )
    .await;
    exec(
        pool,
        "insert into machine_event_marks(episode_id,kind,conversation_id) values($1,'disk_low',$2)",
        &[Uuid::now_v7(), conversation],
    )
    .await;
    exec(
        pool,
        "insert into owner_pushes(id,conversation_id,kind,dedupe_key,payload,run_at)
         values($1,$2,'waiting',$1::text,'{}',now())",
        &[Uuid::now_v7(), conversation],
    )
    .await;
    exec(
        pool,
        "insert into spawn_queue(id,owner_id,parent_id,call_id,spec,reason,not_before)
         values($1,$2,$3,$4,'{}','rate',now())",
        &[Uuid::now_v7(), owner, conversation, Uuid::now_v7()],
    )
    .await;
    exec(
        pool,
        "insert into nudge_rules(conversation_id,owner_id,set_by,set_by_conversation,notify_conversation_id)
         values($1,$2,'agent',$1,$1)",
        &[conversation, owner],
    )
    .await;
    exec(
        pool,
        "insert into nudge_events(id,conversation_id,owner_id,outcome,input_id,signals)
         values($1,$2,$3,'nudged',$4,'{}')",
        &[Uuid::now_v7(), conversation, owner, input],
    )
    .await;
    // Потомок и подписки в обе стороны: после удаления остаются потомок без родителя и ничего больше.
    let delegate = f.conversation().await;
    exec(
        pool,
        "update conversations set parent_conversation_id=$1, spawn_depth=1 where id=$2",
        &[conversation, delegate],
    )
    .await;
    // Доска плана: удаляемый — оркестратор своей задачи (каскад) и исполнитель задачи
    // потомка (связь обнуляется, строка потомка остаётся без ссылки на удалённого).
    exec(
        pool,
        "insert into plan_boards(orchestrator_conversation_id,owner_id,next_number)
         values($1,$2,2),($3,$2,2)",
        &[conversation, owner, delegate],
    )
    .await;
    exec(
        pool,
        "insert into plan_tasks(id,owner_id,orchestrator_conversation_id,number,title,
           executor_conversation_id,state,attempts)
         values($1,$2,$3,1,'task',$4,'in_progress',1),($5,$2,$4,1,'task',$3,'in_progress',1)",
        &[
            Uuid::now_v7(),
            owner,
            conversation,
            delegate,
            Uuid::now_v7(),
        ],
    )
    .await;
    exec(
        pool,
        "insert into session_subscriptions(id,owner_id,subscriber_id,target_id)
         values($1,$2,$3,$4),($5,$2,$4,$3)",
        &[
            Uuid::now_v7(),
            owner,
            conversation,
            delegate,
            Uuid::now_v7(),
        ],
    )
    .await;
    exec(
        pool,
        "insert into scheduled_job_attachments(job_id,attachment_id) values($1,$2)",
        &[job, scheduled],
    )
    .await;
    exec(
        pool,
        "insert into conversation_group_members(group_id,conversation_id) values($1,$2)",
        &[group, conversation],
    )
    .await;
    exec(
        pool,
        "insert into conversation_group_pins(conversation_id,group_id) values($1,$2)",
        &[conversation, group],
    )
    .await;
    sqlx::query(
        "insert into composer_drafts(conversation_id,owner_id,text,revision,device_id)
         values($1,$2,$3,1,'seed')",
    )
    .bind(conversation)
    .bind(owner)
    .bind(format!("черновик {conversation}"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into idempotency_keys(owner_id,key,operation,request_hash,response,conversation_id)
         values($1,$2,'seed',''::bytea,'{}',$3)",
    )
    .bind(owner)
    .bind(format!("seed-{conversation}"))
    .bind(conversation)
    .execute(pool)
    .await
    .unwrap();
    exec(
        pool,
        "insert into limit_snapshots(provider,window_kind,used_permille,observed_at,source,machine_id,execution_id)
         values('claude','weekly',1,now(),'session',$1,$2)",
        &[machine, execution],
    )
    .await;
    exec(
        pool,
        "insert into journal_cursors(machine_id,journal_id,kind,committed_seq,execution_id)
         values($1,$2,'sj',2,$3)",
        &[machine, journal, execution],
    )
    .await;
    let preset = Uuid::now_v7();
    sqlx::query(
        "insert into instruction_presets(id,owner_id,title,revision,origin,origin_conversation_id)
         values($1,$2,$3,1,'agent',$4)",
    )
    .bind(preset)
    .bind(owner)
    .bind(format!("preset-{preset}"))
    .bind(conversation)
    .execute(pool)
    .await
    .unwrap();
    exec(
        pool,
        "insert into instruction_preset_revisions(preset_id,revision,text,edited_by,edited_conversation_id)
         values($1,1,'text','agent',$2)",
        &[preset, conversation],
    )
    .await;
    let mut conn = pool.acquire().await.unwrap();
    for (scope, kind, payload) in [
        (
            Scope::Conversation { id: conversation },
            "input_queued",
            json!({"input_id": input}),
        ),
        (
            Scope::Conversations { owner_id: owner },
            "conversation_updated",
            json!({"id": conversation, "title": format!("секретный заголовок {conversation}")}),
        ),
        (
            Scope::Conversations { owner_id: owner },
            "state_divergence",
            json!({"execution_id": execution, "reason": "unknown_execution"}),
        ),
    ] {
        events::append(
            &mut conn,
            NewEvent {
                scope,
                kind,
                conversation_id: matches!(scope, Scope::Conversation { .. })
                    .then_some(conversation),
                execution_id: None,
                payload,
                correlation_id: Uuid::now_v7(),
            },
        )
        .await
        .unwrap();
    }
    let needles = [
        conversation,
        execution,
        command,
        stored_command,
        cancel,
        input,
        retry,
        block,
        child,
        request,
        job,
        media,
        compaction,
        linked,
        scheduled,
    ]
    .iter()
    .map(Uuid::to_string)
    .chain([
        format!("native-{conversation}"),
        format!("секретный заголовок {conversation}"),
    ])
    .collect();
    Seeded {
        conversation,
        execution,
        journal,
        command,
        stored_command,
        attachments: vec![linked, scheduled],
        media,
        needles,
    }
}

async fn tables(pool: &PgPool) -> Vec<String> {
    sqlx::query_scalar(
        "select tablename::text from pg_tables
         where schemaname='public' and tablename <> '_sqlx_migrations' order by 1",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Таблицы, из которых цепочка внешних ключей приводит к conversations.
async fn fk_chain(pool: &PgPool) -> BTreeSet<String> {
    sqlx::query_scalar::<_, String>(
        "with recursive refs(tbl) as (
            select 'conversations'::regclass
            union
            select c.conrelid::regclass from pg_constraint c
              join refs r on c.confrelid = r.tbl where c.contype = 'f'
         ) select tbl::text from refs",
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .into_iter()
    .collect()
}

/// Строки таблиц, текст которых содержит любой из идентификаторов.
async fn mentions(pool: &PgPool, needles: &[String]) -> BTreeMap<String, Vec<String>> {
    let mut found = BTreeMap::new();
    for table in tables(pool).await {
        let rows: Vec<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            r#"select t::text from "{table}" t
               where exists(select 1 from unnest($1::text[]) n where t::text like '%'||n||'%')
               order by 1"#
        )))
        .bind(needles)
        .fetch_all(pool)
        .await
        .unwrap();
        if !rows.is_empty() {
            found.insert(table, rows);
        }
    }
    found
}

async fn group(f: &Fixture) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("insert into conversation_groups(id,owner_id,name) values($1,$2,'группа')")
        .bind(id)
        .bind(f.owner)
        .execute(&f.pool)
        .await
        .unwrap();
    id
}

async fn trash(f: &Fixture, id: Uuid, key: &str) -> TestResponse {
    f.client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{id}"),
            Some(key),
            None,
        )
        .await
}

async fn purge(f: &Fixture, id: Uuid, key: &str) -> TestResponse {
    f.client
        .send(
            Method::DELETE,
            &format!("/api/v1/conversations/{id}/trash"),
            Some(key),
            None,
        )
        .await
}

/// Старое окончательное удаление: HTTP теперь сначала прячет сессию, поэтому тесты
/// прежнего пути проходят через корзину и сразу окончательную уборку.
async fn delete(f: &Fixture, id: Uuid, key: &str) -> TestResponse {
    let response = trash(f, id, key).await;
    if response.status != 204 {
        return response;
    }
    let purged = purge(f, id, &format!("purge-{key}")).await;
    if purged.status != 204 {
        return purged;
    }
    response
}

fn code(response: &TestResponse) -> String {
    response.json::<Value>()["error"]["code"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

async fn live_in(f: &Fixture, conversation: Uuid, state: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("insert into executions(id,conversation_id,generation,state) values($1,$2,9,$3)")
        .bind(id)
        .bind(conversation)
        .bind(state)
        .execute(&f.pool)
        .await
        .unwrap();
    id
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_leaves_no_row_in_any_table_that_references_the_conversation(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let groups = group(&f).await;
    let victim = seed_everything(&f, f.conversation, f.machine, groups).await;
    let other = f.conversation().await;
    let bystander = seed_everything(&f, other, f.machine, groups).await;

    let seeded = mentions(&f.pool, &victim.needles).await;
    for table in fk_chain(&f.pool).await {
        if matches!(table.as_str(), "instruction_preset_revisions") {
            continue;
        }
        assert!(
            seeded.contains_key(&table),
            "seed misses {table}: the test must cover every table reaching conversations"
        );
    }
    let revisions: i64 = sqlx::query_scalar(
        "select count(*) from instruction_preset_revisions where edited_conversation_id=$1",
    )
    .bind(victim.conversation)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(revisions, 1);
    let untouched = mentions(&f.pool, &bystander.needles).await;

    let response = delete(&f, victim.conversation, "delete-1").await;
    assert_eq!(response.status, 204, "{:?}", response.body);

    let mut left = mentions(&f.pool, &victim.needles).await;
    let tombstones = left.remove("conversation_tombstones").unwrap_or_default();
    assert_eq!(tombstones.len(), 1, "exactly one tombstone");
    assert!(!tombstones[0].contains("секретный"));
    let announced = left.remove("events").unwrap_or_default();
    assert_eq!(announced.len(), 1, "only the deletion event: {announced:?}");
    assert!(announced[0].contains("conversation_deleted"));
    // Подписчик узнаёт об удалении: задание называет только идентификатор.
    let told = left.remove("scheduled_jobs").unwrap_or_default();
    assert_eq!(told.len(), 1, "the delegate's event: {told:?}");
    assert!(told[0].contains("session_task") && told[0].contains("deleted"));
    assert!(!told[0].contains("секретный"));
    assert!(left.is_empty(), "rows still reference it: {left:?}");

    assert_eq!(
        mentions(&f.pool, &bystander.needles).await,
        untouched,
        "the neighbour must not change at all"
    );
    let snapshots: i64 = sqlx::query_scalar("select count(*) from limit_snapshots")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(snapshots, 2, "shared limit history stays");
    let presets: i64 =
        sqlx::query_scalar("select count(*) from instruction_presets where title like 'preset-%'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(presets, 2, "a preset outlives the session that created it");
}

/// Вводы, которые разговор оставил в других сессиях: сообщение, бриф и событие подписки.
async fn sent_to_others(f: &Fixture, sender: Uuid, receiver: Uuid, title: &str, answer: &str) {
    let context = ToolContext {
        pool: f.pool.clone(),
        config: Default::default(),
        limits: Default::default(),
        settle: SettleConfig {
            idle_quiet: std::time::Duration::from_secs(5),
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
    };
    let caller = Caller {
        owner_id: f.owner,
        conversation_id: sender,
        execution_id: Uuid::now_v7(),
        machine_id: f.machine,
        call_id: Uuid::now_v7(),
    };
    session_tools::call(
        &context,
        &caller,
        "sessions.send",
        json!({"session_id": receiver, "text": "тело сообщения"}),
    )
    .await
    .unwrap();
    for (kind, text) in [
        (
            "spawn",
            format!(
                "[canterlot] You were started by session \"{title}\" (id {sender}). \
                 It is an agent, not the owner.\n\nтело брифа"
            ),
        ),
        (
            "event",
            format!(
                "[canterlot] Session \"{title}\" (id {sender}, synthetic, model secret-model) is now idle.\n\
                 Turns: 2, duration 1m 5s, tokens in/out 10/20.\n\
                 Worktree: /w/worktrees/{sender}, branch canterlot/x.\n\
                 Final answer:\n{answer}"
            ),
        ),
    ] {
        submit_input(
            &f.pool,
            SubmitInput {
                owner_id: f.owner,
                conversation_id: receiver,
                idempotency_key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text { text }],
                received_at: chrono::Utc::now(),
                correlation_id: Uuid::now_v7(),
                origin: Some(json!({"kind": kind, "from": sender, "title": title,
                                    "harness": "synthetic", "model": "secret-model",
                                    "chain_depth": 3})),
            },
        )
        .await
        .unwrap();
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_deleted_sender_leaves_no_name_in_other_sessions(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let (sender, receiver, other) = (
        f.conversation().await,
        f.conversation().await,
        f.conversation().await,
    );
    let title = format!("секретный заголовок {sender}");
    for (id, name) in [(sender, title.as_str()), (other, "сосед")] {
        sqlx::query("update conversations set title=$2,model='secret-model' where id=$1")
            .bind(id)
            .bind(name)
            .execute(&f.pool)
            .await
            .unwrap();
    }
    sent_to_others(&f, sender, receiver, &title, "итог работы").await;
    sent_to_others(&f, other, receiver, "сосед", "итог соседа").await;
    // Заметки, ещё ждущие доставки: однострочная о приёме и с узнаваемым заголовком.
    let notes = [
        format!(
            "[canterlot] Session \"{title}\" (id {sender}) was accepted: it is stopped and archived."
        ),
        format!(
            "[canterlot] Session \"{title}\" (id {sender}, synthetic, model secret-model) is now stopped.\n\
             Outcome: stopped.\nFinal answer:\nзаметка работы"
        ),
    ];
    for text in &notes {
        sqlx::query(
            "insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload)
             values($1,'session_task',$2,$3,now()+interval '1 hour',$4)",
        )
        .bind(Uuid::now_v7())
        .bind(f.owner)
        .bind(receiver)
        .bind(json!({"action": "note", "text": text, "origin": {
            "kind": "event", "from": sender, "title": title, "harness": "synthetic",
            "model": "secret-model", "chain_depth": 2, "event": {"kind": "accepted"}}}))
        .execute(&f.pool)
        .await
        .unwrap();
    }
    let needles = vec![title.clone(), sender.to_string()];
    assert!(mentions(&f.pool, &needles).await.contains_key("inputs"));

    let response = delete(&f, sender, "delete-sender").await;
    assert_eq!(response.status, 204, "{:?}", response.body);

    let mut left = mentions(&f.pool, &needles).await;
    assert_eq!(
        left.remove("conversation_tombstones").map(|t| t.len()),
        Some(1)
    );
    let announced = left.remove("events").unwrap_or_default();
    assert_eq!(announced.len(), 1, "only the deletion event: {announced:?}");
    assert!(announced[0].contains("conversation_deleted"));
    assert!(left.is_empty(), "the name survives in {left:?}");
    // Текст итога остаётся у получателя: ввод, его команда и событие очереди. Больше нигде.
    let kept = mentions(&f.pool, &["итог работы".to_owned()]).await;
    assert_eq!(
        kept.keys().map(String::as_str).collect::<Vec<_>>(),
        ["commands", "events", "inputs"],
        "{kept:?}"
    );
    assert!(kept.values().all(|rows| rows.len() == 1), "{kept:?}");

    let rows: Vec<(Value, Value, Value)> = sqlx::query_as(
        "select i.content,i.origin,c.payload->'content' from inputs i
         join commands c on c.id=i.command_id
         where i.conversation_id=$1 and i.origin ? 'sender_deleted' order by i.input_seq",
    )
    .bind(receiver)
    .fetch_all(&f.pool)
    .await
    .unwrap();
    let texts: Vec<&str> = rows
        .iter()
        .map(|(content, _, _)| content[0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        [
            "[canterlot] Message from a session that was deleted. \
             This is another agent, not the owner.\n\nтело сообщения",
            "[canterlot] You were started by a session that was deleted. \
             It was an agent, not the owner.\n\nтело брифа",
            "[canterlot] Report from a delegate session that was later deleted. \
             It was an agent, not the owner.\nThe session is now idle.\n\
             Turns: 2, duration 1m 5s, tokens in/out 10/20.\n\
             Worktree: /w/worktrees/(deleted), branch canterlot/x.\n\
             Final answer:\nитог работы",
        ]
    );
    let kinds = ["message", "spawn", "event"];
    for ((content, origin, command), kind) in rows.iter().zip(kinds) {
        assert_eq!(
            content, command,
            "the queued command is what the agent gets"
        );
        let depth = if kind == "message" { 1 } else { 3 };
        assert_eq!(
            origin,
            &json!({"kind": kind, "chain_depth": depth, "sender_deleted": true})
        );
    }
    let queued: Vec<Value> = sqlx::query_scalar(
        "select payload from events where conversation_id=$1 and kind='input_queued'
         and payload->'origin' ? 'sender_deleted' order by scope_seq",
    )
    .bind(receiver)
    .fetch_all(&f.pool)
    .await
    .unwrap();
    assert_eq!(queued.len(), 3);
    for (event, (content, origin, _)) in queued.iter().zip(&rows) {
        assert_eq!(&event["content"], content);
        assert_eq!(&event["origin"], origin);
    }
    let pending: Vec<Value> = sqlx::query_scalar(
        "select payload from scheduled_jobs where conversation_id=$1
         and payload->>'action'='note' order by id",
    )
    .bind(receiver)
    .fetch_all(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        pending,
        [
            json!({"action": "note",
                   "text": "[canterlot] A session this conversation heard from was deleted.",
                   "origin": {"kind": "event", "chain_depth": 2, "sender_deleted": true}}),
            json!({"action": "note",
                   "text": "[canterlot] Report from a delegate session that was later deleted. \
                            It was an agent, not the owner.\nThe session is now stopped.\n\
                            Outcome: stopped.\nFinal answer:\nзаметка работы",
                   "origin": {"kind": "event", "chain_depth": 2, "sender_deleted": true}}),
        ]
    );

    let neighbour: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1 and origin->>'title'='сосед'
         and origin->>'from'=$2",
    )
    .bind(receiver)
    .bind(other.to_string())
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(neighbour, 3, "another sender keeps its name");
}

/// Как удаляется делегат: корзина, ручное удаление, срок хранения принятых.
#[derive(Clone, Copy, Debug)]
enum Removal {
    Trash,
    Manual,
    AcceptedRetention,
}

/// Итог делегата остаётся у родителя без строки с названием при любом пути удаления.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_delegate_result_keeps_its_text_in_the_parent_after_any_delete(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for removal in [Removal::Trash, Removal::Manual, Removal::AcceptedRetention] {
        let parent = f.conversation().await;
        let child = f.conversation().await;
        let title = format!("делегат-секрет {child}");
        sqlx::query(
            "update conversations set title=$2,model='secret-model',parent_conversation_id=$3,
                                      spawn_depth=1 where id=$1",
        )
        .bind(child)
        .bind(&title)
        .bind(parent)
        .execute(&f.pool)
        .await
        .unwrap();
        submit_input(
            &f.pool,
            SubmitInput {
                owner_id: f.owner,
                conversation_id: parent,
                idempotency_key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text {
                    text: format!(
                        "[canterlot] Session \"{title}\" (id {child}, synthetic, model secret-model) is now idle.\n\
                         Final answer:\nитог {removal:?}"
                    ),
                }],
                received_at: chrono::Utc::now(),
                correlation_id: Uuid::now_v7(),
                origin: Some(json!({"kind": "event", "from": child, "title": title,
                                    "harness": "synthetic", "model": "secret-model",
                                    "chain_depth": 0, "event": {"kind": "session_result"}})),
            },
        )
        .await
        .unwrap();
        match removal {
            Removal::Trash => {
                let response = trash(&f, child, &format!("trash-{child}")).await;
                assert_eq!(response.status, 204, "{:?}", response.body);
            }
            Removal::Manual => canterlot_hub::deletion::delete(
                &f.pool,
                f.store.as_ref(),
                f.owner,
                child,
                &format!("delete-{child}"),
                Uuid::now_v7(),
            )
            .await
            .unwrap(),
            Removal::AcceptedRetention => {
                sqlx::query("update conversations set archived_at=$2,accepted_at=$2 where id=$1")
                    .bind(child)
                    .bind(chrono::Utc::now() - chrono::TimeDelta::days(4))
                    .execute(&f.pool)
                    .await
                    .unwrap();
                let (dispatcher, _) = canterlot_hub::dispatcher::Dispatcher::channel();
                let purged = canterlot_hub::scheduler::Scheduler::new(
                    f.pool.clone(),
                    dispatcher,
                    Default::default(),
                )
                .purge_accepted_retention(chrono::Utc::now())
                .await
                .unwrap();
                assert_eq!(purged, 1);
            }
        }
        let (content, origin, command): (Value, Value, Value) = sqlx::query_as(
            "select i.content,i.origin,c.payload->'content' from inputs i
             join commands c on c.id=i.command_id where i.conversation_id=$1",
        )
        .bind(parent)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            content[0]["text"],
            format!(
                "[canterlot] Report from a delegate session that was later deleted. \
                 It was an agent, not the owner.\nThe session is now idle.\n\
                 Final answer:\nитог {removal:?}"
            ),
            "{removal:?}"
        );
        assert_eq!(content, command, "{removal:?}");
        assert_eq!(
            origin,
            json!({"kind": "event", "chain_depth": 0, "sender_deleted": true}),
            "{removal:?}"
        );
        let named: i64 = sqlx::query_scalar(
            "select count(*) from inputs i join commands c on c.id=i.command_id
             where i.conversation_id=$1
               and (i::text like '%'||$2||'%' or c.payload::text like '%'||$2||'%')",
        )
        .bind(parent)
        .bind(child.to_string())
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(
            named, 0,
            "{removal:?}: the parent keeps no id or title of the child"
        );
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_frees_the_files_of_the_session_and_no_others(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let groups = group(&f).await;
    let victim = seed_everything(&f, f.conversation, f.machine, groups).await;
    let other = f.conversation().await;
    let bystander = seed_everything(&f, other, f.machine, groups).await;
    let staged = {
        let bytes = content(64, 3);
        let response = f
            .create_in(victim.conversation, "c.txt", "text/plain", &bytes)
            .await;
        let id = id_of(&response);
        let half = bytes.len() / 2;
        f.patch_bytes(id, 0, &bytes[..half]).await;
        id
    };
    let files = |seeded: &Seeded| {
        let mut ids = seeded.attachments.clone();
        ids.push(seeded.media);
        ids
    };
    for id in files(&victim).into_iter().chain(files(&bystander)) {
        assert!(f.blob_path(id).exists(), "{id}");
    }
    assert!(f.staging_path(staged).exists());

    let response = delete(&f, victim.conversation, "delete-files").await;
    assert_eq!(response.status, 204, "{:?}", response.body);

    for id in files(&victim) {
        assert!(!f.blob_path(id).exists(), "{id} must be freed");
    }
    assert!(!f.staging_path(staged).exists(), "staging must be freed");
    for id in files(&bystander) {
        assert!(f.blob_path(id).exists(), "{id} belongs to a neighbour");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn retry_replays_and_a_new_request_gets_a_clear_not_found(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.conversation;
    assert_eq!(trash(&f, id, "same").await.status, 204);
    assert_eq!(trash(&f, id, "same").await.status, 204, "same key replays");
    assert_eq!(trash(&f, id, "other").await.status, 204, "already trashed");
    assert_eq!(purge(&f, id, "purge").await.status, 204);
    assert_eq!(purge(&f, id, "purge").await.status, 204, "same key replays");
    let again = trash(&f, id, "after").await;
    assert_eq!(again.status, 404);
    assert_eq!(code(&again), "conversation_deleted");
    let unknown = trash(&f, Uuid::now_v7(), "unknown").await;
    assert_eq!(unknown.status, 404);
    assert_eq!(code(&unknown), "not_found");
    let events: i64 =
        sqlx::query_scalar("select count(*) from events where kind='conversation_deleted'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(events, 1);
    let tombstones: i64 = sqlx::query_scalar("select count(*) from conversation_tombstones")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(tombstones, 1);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_key_of_another_request_is_a_conflict(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let other = f.conversation().await;
    assert_eq!(delete(&f, f.conversation, "shared").await.status, 204);
    let response = delete(&f, other, "shared").await;
    assert_eq!(response.status, 409);
    assert_eq!(code(&response), "idempotency_conflict");
    let alive: bool = sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
        .bind(other)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert!(alive);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn every_live_state_refuses_and_changes_nothing(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for state in ["created", "starting", "running", "stopping", "owner_lost"] {
        let id = f.conversation().await;
        live_in(&f, id, state).await;
        let needles = [id.to_string()];
        let before = mentions(&f.pool, &needles).await;
        let response = delete(&f, id, &format!("live-{state}")).await;
        assert_eq!(response.status, 409, "{state}");
        assert_eq!(code(&response), "conversation_live", "{state}");
        assert_eq!(mentions(&f.pool, &needles).await, before, "{state}");
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_conversation_of_another_owner_is_invisible(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let stranger = Uuid::now_v7();
    exec(&f.pool, "insert into owners(id) values($1)", &[stranger]).await;
    let machine = Uuid::now_v7();
    exec(
        &f.pool,
        "insert into machines(id,owner_id,name) values($1,$2,'чужая')",
        &[machine, stranger],
    )
    .await;
    let foreign = testkit::seed_conversation(&f.pool, stranger, machine).await;
    let response = delete(&f, foreign, "foreign").await;
    assert_eq!(response.status, 404);
    assert_eq!(code(&response), "not_found");
    let alive: bool = sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
        .bind(foreign)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert!(alive);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_reference_from_a_neighbour_hands_the_attachment_over(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let groups = group(&f).await;
    let victim = seed_everything(&f, f.conversation, f.machine, groups).await;
    let other = f.conversation().await;
    let neighbour = seed_everything(&f, other, f.machine, groups).await;
    let input: Uuid = sqlx::query_scalar("select id from inputs where command_id=$1")
        .bind(neighbour.command)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    let shared = victim.attachments[0];
    exec(
        &f.pool,
        "insert into input_attachments(input_id,attachment_id,position) values($1,$2,7)",
        &[input, shared],
    )
    .await;
    assert_eq!(
        trash(&f, victim.conversation, "shared-reference")
            .await
            .status,
        204
    );
    let response = purge(&f, victim.conversation, "shared-reference-purge").await;
    assert_eq!(response.status, 204, "{:?}", response.body);
    // Вложение, на которое ссылается ввод соседа, переходит к нему вместе с blob;
    // от удалённой сессии остаётся только сам файл в строках вложения и ссылки соседа.
    let holder: Uuid = sqlx::query_scalar("select conversation_id from attachments where id=$1")
        .bind(shared)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(holder, other);
    assert!(f.blob_path(shared).exists());
    let mut left = mentions(&f.pool, &victim.needles).await;
    assert_eq!(
        left.remove("conversation_tombstones").map(|t| t.len()),
        Some(1)
    );
    let announced = left.remove("events").unwrap_or_default();
    assert_eq!(announced.len(), 1, "only the deletion event: {announced:?}");
    assert!(announced[0].contains("conversation_deleted"));
    let told = left.remove("scheduled_jobs").unwrap_or_default();
    assert!(
        told.iter()
            .all(|job| job.contains("session_task") && job.contains("deleted"))
    );
    assert!(
        left.keys()
            .all(|table| table == "attachments" || table == "input_attachments"),
        "{left:#?}"
    );
    for rows in left.values() {
        assert!(
            rows.iter().all(|row| row.contains(&shared.to_string())),
            "{left:#?}"
        );
        assert!(
            rows.iter()
                .all(|row| !row.contains(&victim.conversation.to_string())),
            "{left:#?}"
        );
    }
    for id in &victim.attachments[1..] {
        assert!(!f.blob_path(*id).exists());
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn other_devices_learn_through_the_event_flow(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let groups = group(&f).await;
    let victim = seed_everything(&f, f.conversation, f.machine, groups).await;
    let other = f.conversation().await;
    let scope = Scope::Conversations { owner_id: f.owner }.key();
    let head: i64 = sqlx::query_scalar("select last_seq from scope_counters where scope=$1")
        .bind(&scope)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(delete(&f, victim.conversation, "evented").await.status, 204);

    let page: Value = f
        .client
        .send(
            Method::GET,
            "/api/v1/events?scope=conversations&after=0&limit=100",
            None,
            None,
        )
        .await
        .json();
    let events = page["events"].as_array().unwrap();
    let last = events.last().unwrap();
    assert_eq!(last["kind"], "conversation_deleted");
    assert_eq!(last["conversation_id"], victim.conversation.to_string());
    assert_eq!(last["payload"]["id"], victim.conversation.to_string());
    // Остаются соседняя сессия и потомок удалённой.
    assert_eq!(last["payload"]["counts"]["all"], 2);
    // Корзина оставила в журнале обновление, окончательная уборка спрятала его и добавила удаление.
    assert_eq!(last["scope_seq"], (head + 2).to_string());
    assert!(
        events[..events.len() - 1]
            .iter()
            .filter(|event| event["kind"] != "limits_providers_changed")
            .all(|event| event["kind"] == "hidden"),
        "the old rows lose their content but keep the sequence: {events:?}"
    );
    assert_eq!(events[0]["scope_seq_from"], "1");

    let list: Value = f
        .client
        .send(Method::GET, "/api/v1/snapshot/conversations", None, None)
        .await
        .json();
    let ids: Vec<&str> = list["data"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), 2, "the neighbour and the orphaned delegate");
    assert!(ids.contains(&other.to_string().as_str()));
    assert!(!ids.contains(&victim.conversation.to_string().as_str()));
    let scope_row: bool = sqlx::query_scalar(
        "select exists(select 1 from scope_counters where scope=$1 or scope like $2)",
    )
    .bind(format!("conversation:{}", victim.conversation))
    .bind(format!("%{}%", victim.conversation))
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert!(!scope_row);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_deleted_conversation_answers_conversation_deleted_everywhere(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let id = f.conversation;
    assert_eq!(delete(&f, id, "gone").await.status, 204);
    for (method, path, body) in [
        (Method::GET, format!("/api/v1/conversations/{id}"), None),
        (
            Method::GET,
            format!("/api/v1/conversations/{id}/snapshot"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{id}/inputs"),
            Some(json!({"content":[{"type":"text","text":"поздно"}]})),
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{id}/executions"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{id}/resume"),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{id}/interrupt"),
            None,
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{id}/stop"),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("/api/v1/conversations/{id}/compact"),
            None,
        ),
        (
            Method::PATCH,
            format!("/api/v1/conversations/{id}/settings"),
            Some(json!({"effort":"high"})),
        ),
        (
            Method::POST,
            format!(
                "/api/v1/conversations/{id}/inputs/{}/send-now",
                Uuid::now_v7()
            ),
            None,
        ),
    ] {
        let key = (method != Method::GET).then(|| Uuid::now_v7().to_string());
        let response = f.client.send(method, &path, key.as_deref(), body).await;
        assert_eq!(response.status, 404, "{path}: {:?}", response.body);
        assert_eq!(code(&response), "conversation_deleted", "{path}");
    }
    let revived: i64 = sqlx::query_scalar("select count(*) from conversations where id=$1")
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(revived, 0);
}

/// Машина с действующим токеном и разговором на ней; исполнение остановлено.
async fn machine_with_session(f: &Fixture) -> (machines::NewMachine, Vec<u8>, Seeded) {
    use sha2::Digest as _;
    let machine = machines::add(&f.pool, f.owner, "worker").await.unwrap();
    let digest = sha2::Sha256::digest(machine.token.as_bytes()).to_vec();
    let conversation = testkit::seed_conversation(&f.pool, f.owner, machine.machine_id).await;
    let groups = group(f).await;
    let seeded = seed_everything(f, conversation, machine.machine_id, groups).await;
    (machine, digest, seeded)
}

fn journal(id: Uuid, first: u64, bodies: Vec<record_body::Body>) -> JournalRecords {
    JournalRecords {
        journal_id: id.to_string(),
        records: bodies
            .into_iter()
            .enumerate()
            .map(|(i, body)| JournalRecord {
                seq: first + i as u64,
                body: Some(RecordBody { body: Some(body) }),
            })
            .collect(),
    }
}

fn delta(block: Uuid) -> record_body::Body {
    record_body::Body::BlockDelta(BlockDelta {
        block_id: block.to_string(),
        base_rev: 1,
        rev: 2,
        append: "поздний текст".into(),
    })
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn late_journal_records_are_acknowledged_and_change_nothing(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let (machine, digest, seeded) = machine_with_session(&f).await;
    assert_eq!(delete(&f, seeded.conversation, "late").await.status, 204);
    let ingest = Ingest::new(f.pool.clone());
    let outcome = ingest
        .apply_batch(
            machine.machine_id,
            &digest,
            JournalKind::Sj,
            &journal(seeded.journal, 3, vec![delta(Uuid::now_v7())]),
        )
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Committed { up_to_seq: 3 });
    let replay = ingest
        .apply_batch(
            machine.machine_id,
            &digest,
            JournalKind::Sj,
            &journal(seeded.journal, 1, vec![delta(Uuid::now_v7())]),
        )
        .await
        .unwrap();
    assert_eq!(
        replay,
        IngestOutcome::Committed { up_to_seq: 3 },
        "a replay from the start stops at the cursor"
    );
    let needles = [seeded.conversation.to_string()];
    let left = mentions(&f.pool, &needles).await;
    assert_eq!(
        left.keys().collect::<Vec<_>>(),
        ["conversation_tombstones", "events", "scheduled_jobs"],
        "the job is the subscriber's event about the deletion"
    );
    let conversation_events: i64 =
        sqlx::query_scalar("select count(*) from events where scope like 'conversation:%'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(conversation_events, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_journal_first_seen_after_delete_is_acknowledged(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let (machine, digest, seeded) = machine_with_session(&f).await;
    assert_eq!(delete(&f, seeded.conversation, "late").await.status, 204);
    let unseen = Uuid::now_v7();
    let opened = record_body::Body::JournalOpened(JournalOpened {
        execution_id: seeded.execution.to_string(),
        generation: 1,
        supervisor_protocol_version: 1,
    });
    let outcome = Ingest::new(f.pool.clone())
        .apply_batch(
            machine.machine_id,
            &digest,
            JournalKind::Sj,
            &journal(unseen, 1, vec![opened, delta(Uuid::now_v7())]),
        )
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Committed { up_to_seq: 2 });
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_journal_of_a_truly_unknown_execution_is_still_refused(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let (machine, digest, seeded) = machine_with_session(&f).await;
    assert_eq!(delete(&f, seeded.conversation, "late").await.status, 204);
    let opened = record_body::Body::JournalOpened(JournalOpened {
        execution_id: Uuid::now_v7().to_string(),
        generation: 1,
        supervisor_protocol_version: 1,
    });
    let outcome = Ingest::new(f.pool.clone())
        .apply_batch(
            machine.machine_id,
            &digest,
            JournalKind::Sj,
            &journal(Uuid::now_v7(), 1, vec![opened]),
        )
        .await;
    assert!(outcome.is_err(), "{outcome:?}");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn late_control_facts_are_acknowledged(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let (machine, digest, seeded) = machine_with_session(&f).await;
    assert_eq!(delete(&f, seeded.conversation, "late").await.status, 204);
    let facts = vec![
        record_body::Body::CommandStored(CommandStored {
            machine_time_ms: 0,
            command_id: seeded.stored_command.to_string(),
            dispatch_attempt: 1,
        }),
        record_body::Body::InputRejected(InputRejected {
            command_id: seeded.stored_command.to_string(),
            dispatch_attempt: 1,
            reason: "generation_closed".into(),
        }),
        record_body::Body::ExecutionState(canterlot_proto::worker::v1::ExecutionState {
            execution_id: seeded.execution.to_string(),
            generation: 1,
            state: "stopped".into(),
            outcome: "user_stop".into(),
            ..Default::default()
        }),
    ];
    let outcome = Ingest::new(f.pool.clone())
        .apply_batch(
            machine.machine_id,
            &digest,
            JournalKind::Cj,
            &journal(Uuid::now_v7(), 1, facts),
        )
        .await
        .unwrap();
    assert_eq!(outcome, IngestOutcome::Committed { up_to_seq: 3 });
    let command: i64 = sqlx::query_scalar("select count(*) from commands where id=$1")
        .bind(seeded.command)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(command, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn reconcile_is_quiet_about_a_deleted_execution(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let (machine, _, seeded) = machine_with_session(&f).await;
    assert_eq!(delete(&f, seeded.conversation, "late").await.status, 204);
    Ingest::new(f.pool.clone())
        .reconcile(
            machine.machine_id,
            &ReconcileReport {
                executions: vec![ExecutionObservation {
                    execution_id: seeded.execution.to_string(),
                    generation: 1,
                    state: "stopped".into(),
                    outcome: "user_stop".into(),
                    supervisor_attached: false,
                    sj_journal_id: seeded.journal.to_string(),
                }],
            },
        )
        .await
        .unwrap();
    let divergences: i64 = sqlx::query_scalar(
        "select count(*) from events where kind='state_divergence' and payload::text like $1",
    )
    .bind(format!("%{}%", seeded.execution))
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(divergences, 0);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delete_racing_a_start_never_loses_either_side(pool: PgPool) {
    let f = Fixture::new(pool).await;
    for round in 0..20 {
        let id = f.conversation().await;
        let (delete_key, start_key) = (
            format!("race-delete-{round}"),
            format!("race-start-{round}"),
        );
        let path = format!("/api/v1/conversations/{id}/resume");
        let (deleted, started) = tokio::join!(
            delete(&f, id, &delete_key),
            f.client
                .send(Method::POST, &path, Some(&start_key), Some(json!({}))),
        );
        let conversations: i64 =
            sqlx::query_scalar("select count(*) from conversations where id=$1")
                .bind(id)
                .fetch_one(&f.pool)
                .await
                .unwrap();
        let live = sqlx::query_scalar::<_, i64>(
            "select count(*) from executions where conversation_id=$1 and state='created'",
        )
        .bind(id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        match (deleted.status.as_u16(), started.status.as_u16()) {
            (204, 404) => assert_eq!((conversations, live), (0, 0)),
            (409, 201) => assert_eq!((conversations, live), (1, 1)),
            other => panic!("unexpected race outcome {other:?}"),
        }
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_stopped_session_with_a_real_execution_can_be_deleted(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let started = executions::start(&f.pool, f.owner, f.conversation, Uuid::now_v7())
        .await
        .unwrap();
    assert_eq!(
        code(&delete(&f, f.conversation, "while-live").await),
        "conversation_live"
    );
    executions::set_state(
        &f.pool,
        started.execution_id,
        ExecutionState::Failed,
        Some("start_rejected"),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(delete(&f, f.conversation, "after-stop").await.status, 204);
}

/// Текст правки отложенного сообщения лежит в ответе строки идемпотентности, поэтому
/// окончательное удаление должно уносить её вместе с остальными данными разговора,
/// а не ждать семидневной уборки планировщика.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn deleting_a_conversation_removes_scheduled_message_text_from_idempotency(pool: PgPool) {
    use canterlot_hub::{
        config::SchedulerConfig,
        scheduler::{self, NewScheduled, PatchScheduled},
    };
    use chrono::{TimeDelta, Utc};

    let f = Fixture::new(pool).await;
    // Планировщик сервиса нужен, чтобы правка шла тем же путём, что и из HTTP.
    let _scheduler = scheduler::Scheduler::new(
        f.pool.clone(),
        canterlot_hub::dispatcher::Dispatcher::channel().0,
        SchedulerConfig::default(),
    );
    let now = Utc::now();
    let text = |text: &str| vec![ContentBlock::Text { text: text.into() }];
    let message = scheduler::schedule_message(
        &f.pool,
        f.owner,
        "sched-create",
        NewScheduled {
            conversation_id: f.conversation,
            run_at: now + TimeDelta::hours(1),
            content: text("секретный черновик"),
            attachment_ids: vec![],
        },
        now,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    scheduler::update_message(
        &f.pool,
        f.owner,
        message.id,
        "sched-edit",
        PatchScheduled {
            run_at: None,
            content: Some(text("секретный черновик")),
        },
        now,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let bound: Option<Uuid> =
        sqlx::query_scalar("select conversation_id from idempotency_keys where key='sched-edit'")
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(
        bound,
        Some(f.conversation),
        "the key names its conversation"
    );

    assert_eq!(delete(&f, f.conversation, "delete-sched").await.status, 204);
    let left: i64 = sqlx::query_scalar(
        "select count(*) from idempotency_keys where key='sched-edit'
            or response::text like '%секретный черновик%'",
    )
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(left, 0, "no scheduled text survives the conversation");
}
