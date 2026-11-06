use std::{sync::Arc, time::Duration};

use canterlot_hub::{
    activity::{self, Thresholds},
    config::SessionToolsConfig,
    executions,
    session_tools::{
        self, Caller, ToolContext, ToolError, ToolErrorCode as Code, settle::SettleConfig,
    },
    testkit,
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

struct Fx {
    pool: PgPool,
    context: ToolContext,
    owner: Uuid,
    machine: Uuid,
    root: Uuid,
}

impl Fx {
    async fn new(pool: PgPool) -> Self {
        let seed = testkit::seed(&pool).await;
        Self {
            context: ToolContext {
                pool: pool.clone(),
                config: Arc::new(SessionToolsConfig::default()),
                limits: Default::default(),
                settle: SettleConfig {
                    idle_quiet: Duration::from_secs(5),
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
            root: seed.conversation_id,
        }
    }

    async fn conversation(&self, title: &str, parent: Option<Uuid>, depth: i32) -> Uuid {
        let id = testkit::seed_conversation(&self.pool, self.owner, self.machine).await;
        sqlx::query(
            "update conversations set title=$2,parent_conversation_id=$3,spawn_depth=$4,model='small'
             where id=$1",
        )
        .bind(id)
        .bind(title)
        .bind(parent)
        .bind(depth)
        .execute(&self.pool)
        .await
        .unwrap();
        self.refresh(id).await;
        id
    }

    async fn refresh(&self, id: Uuid) {
        activity::refresh(
            &self.pool,
            id,
            Utc::now(),
            &Thresholds {
                stalled_generating: Duration::from_secs(300),
                stalled_tool: Duration::from_secs(1800),
                idle_quiet: Duration::from_secs(5),
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap();
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

    async fn listed(&self, from: Uuid, args: Value) -> Vec<Uuid> {
        self.call(from, "sessions.list", args).await.unwrap()["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| serde_json::from_value(s["id"].clone()).unwrap())
            .collect()
    }

    async fn visible(&self, from: Uuid, id: Uuid) -> bool {
        match self
            .call(from, "sessions.inspect", json!({"session_id": id}))
            .await
        {
            Ok(_) => true,
            Err(error) => {
                assert_eq!(error.code, Code::SessionNotFound);
                false
            }
        }
    }
}

/// Семья: root -> parent -> child -> grandchild, рядом сосед и посторонний.
struct Family {
    parent: Uuid,
    child: Uuid,
    grandchild: Uuid,
    sibling: Uuid,
    stranger: Uuid,
}

async fn family(f: &Fx) -> Family {
    let parent = f.conversation("родитель", Some(f.root), 1).await;
    let child = f.conversation("ребёнок", Some(parent), 2).await;
    let grandchild = f.conversation("внук", Some(child), 3).await;
    let sibling = f.conversation("сосед", Some(parent), 2).await;
    let stranger = f.conversation("посторонний", None, 0).await;
    Family {
        parent,
        child,
        grandchild,
        sibling,
        stranger,
    }
}

fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.sort();
    ids
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn root_sees_all_delegated_sees_family_only(pool: PgPool) {
    let f = Fx::new(pool).await;
    let family = family(&f).await;
    let everyone = sorted(vec![
        f.root,
        family.parent,
        family.child,
        family.grandchild,
        family.sibling,
        family.stranger,
    ]);
    assert_eq!(sorted(f.listed(f.root, json!({})).await), everyone);
    assert_eq!(sorted(f.listed(family.stranger, json!({})).await), everyone);
    assert_eq!(
        sorted(f.listed(family.child, json!({})).await),
        sorted(vec![f.root, family.parent, family.child, family.grandchild]),
        "itself, ancestors and descendants, not the sibling"
    );
    assert_eq!(
        sorted(f.listed(family.parent, json!({})).await),
        sorted(vec![
            f.root,
            family.parent,
            family.child,
            family.grandchild,
            family.sibling
        ])
    );

    let other = testkit::seed(&f.pool).await;
    for from in [f.root, family.child] {
        assert!(
            !f.listed(from, json!({"limit": 50}))
                .await
                .contains(&other.conversation_id)
        );
        assert!(!f.visible(from, other.conversation_id).await);
    }
    let row = &f
        .call(family.child, "sessions.list", json!({}))
        .await
        .unwrap()["sessions"];
    let me = row
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == json!(family.child))
        .unwrap();
    assert_eq!(me["is_self"], true);
    assert_eq!(me["title"], "ребёнок");
    assert_eq!(me["parent_id"], json!(family.parent));
    assert_eq!(me["harness"], "synthetic");
    assert_eq!(me["model"], "small");
    assert_eq!(me["workdir"], "/tmp/w");
    assert_eq!(me["queued"], 0);
    assert_eq!(me["worktree"], Value::Null);
    assert_eq!(me["activity"]["state"], "idle");
    assert!(me["activity"]["since"].is_string());
    assert_eq!(me["activity"]["evidence"]["kind"], "idle");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delegated_cannot_inspect_stranger(pool: PgPool) {
    let f = Fx::new(pool).await;
    let family = family(&f).await;
    let stranger = f
        .call(
            family.child,
            "sessions.inspect",
            json!({"session_id": family.stranger}),
        )
        .await
        .unwrap_err();
    let unknown = f
        .call(
            family.child,
            "sessions.inspect",
            json!({"session_id": Uuid::now_v7()}),
        )
        .await
        .unwrap_err();
    assert_eq!(stranger, unknown, "the same error as an unknown id");
    assert_eq!(stranger.code, Code::SessionNotFound);
    assert!(!f.visible(family.child, family.sibling).await);
    assert!(f.visible(family.child, family.child).await);
    assert!(f.visible(family.child, family.parent).await);
    assert!(f.visible(family.child, f.root).await);
    assert!(f.visible(family.child, family.grandchild).await);
    assert!(f.visible(family.parent, family.grandchild).await);
    assert!(f.visible(family.grandchild, f.root).await);

    let send = f
        .call(
            family.child,
            "sessions.send",
            json!({"session_id": family.stranger, "text": "эй"}),
        )
        .await
        .unwrap_err();
    assert_eq!(send, unknown);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_delegate_stays_narrow_after_its_parent_is_deleted(pool: PgPool) {
    let f = Fx::new(pool).await;
    let family = family(&f).await;
    // Так оставляет deletion.rs: потомок без родителя, но всё ещё делегированный.
    sqlx::query("update conversations set parent_conversation_id=null where id=$1")
        .bind(family.child)
        .execute(&f.pool)
        .await
        .unwrap();
    assert_eq!(
        sorted(f.listed(family.child, json!({})).await),
        sorted(vec![family.child, family.grandchild]),
    );
    assert!(!f.visible(family.child, family.stranger).await);
    assert!(!f.visible(family.child, family.parent).await);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn list_filters_limits_and_hides_archived(pool: PgPool) {
    let f = Fx::new(pool).await;
    let archived = f.conversation("в архиве", None, 0).await;
    sqlx::query("update conversations set archived_at=now() where id=$1")
        .bind(archived)
        .execute(&f.pool)
        .await
        .unwrap();
    let working = f.conversation("работает", None, 0).await;
    sqlx::query(
        "update conversation_derived set activity_state='stalled' where conversation_id=$1",
    )
    .bind(working)
    .execute(&f.pool)
    .await
    .unwrap();
    assert!(!f.listed(f.root, json!({})).await.contains(&archived));
    assert!(
        f.listed(f.root, json!({"include_archived": true}))
            .await
            .contains(&archived)
    );
    assert_eq!(
        f.listed(f.root, json!({"state": "stalled"})).await,
        vec![working]
    );
    assert_eq!(f.listed(f.root, json!({"limit": 1})).await.len(), 1);
    for bad in [
        json!({"limit": 0}),
        json!({"limit": 51}),
        json!({"state": "nonsense"}),
        json!({"extra": true}),
    ] {
        assert_eq!(
            f.call(f.root, "sessions.list", bad).await.unwrap_err().code,
            Code::InvalidArguments
        );
    }
}

async fn block(f: &Fx, conversation: Uuid, kind: &str, text: &str) -> Uuid {
    let id = Uuid::now_v7();
    let seq: i64 = sqlx::query_scalar(
        "select coalesce(max(created_seq),0)+1 from blocks where conversation_id=$1",
    )
    .bind(conversation)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let execution: Uuid =
        sqlx::query_scalar("select id from executions where conversation_id=$1 limit 1")
            .bind(conversation)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    sqlx::query(
        "insert into blocks(block_id,conversation_id,execution_id,kind,agent,model,state,created_seq,content,rev)
         values($1,$2,$3,$4,'main','small','final',$5,$6,1)",
    )
    .bind(id)
    .bind(conversation)
    .bind(execution)
    .bind(kind)
    .bind(seq)
    .bind(text)
    .execute(&f.pool)
    .await
    .unwrap();
    id
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn inspect_truncates_and_redacts(pool: PgPool) {
    let f = Fx::new(pool).await;
    let target = f.conversation("цель", None, 0).await;
    let execution = executions::start(&f.pool, f.owner, target, Uuid::now_v7())
        .await
        .unwrap();
    for _ in 0..19 {
        block(&f, target, "text", &"ю".repeat(10_000)).await;
    }
    let last = block(&f, target, "text", "короткий хвост").await;
    sqlx::query(
        "insert into requests(id,conversation_id,execution_id,generation,native_request_id,kind,state,request_payload,decision_payload,created_seq)
         values($1,$2,$3,$6,'n','question','open',$4,$5,100)",
    )
    .bind(Uuid::now_v7())
    .bind(target)
    .bind(execution.execution_id)
    .bind(json!({"questions":[{"id":"q","text":"Какой пароль?","is_secret":true}]}))
    .bind(json!({"answers":{"q":"СЕКРЕТНЫЙ-ОТВЕТ"}}))
    .bind(execution.generation)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into machine_metrics(machine_id,machine_time_ms,cpu_cores,cpu_permille,mem_total_bytes,mem_available_bytes,load1_centi,load5_centi,load15_centi,disks,executions)
         values($1,1,4,100,1,1,1,1,1,'[]',$2)",
    )
    .bind(f.machine)
    .bind(json!([
        {"execution_id": execution.execution_id, "alive": true, "cpu_permille": 250, "rss_bytes": 4096},
        {"execution_id": Uuid::now_v7(), "alive": true, "cpu_permille": 999, "rss_bytes": 1}
    ]))
    .execute(&f.pool)
    .await
    .unwrap();
    f.refresh(target).await;

    let answer = f
        .call(
            f.root,
            "sessions.inspect",
            json!({"session_id": target, "messages": 20}),
        )
        .await
        .unwrap();
    let text = answer.to_string();
    assert!(!text.contains("СЕКРЕТНЫЙ-ОТВЕТ"), "{}", text.len());
    assert!(!text.contains("decision"), "no decision reaches a model");

    let messages = answer["messages"].as_array().unwrap();
    let bytes: usize = messages
        .iter()
        .map(|m| m["text"].as_str().unwrap_or_default().len())
        .sum();
    assert!(bytes <= 64 * 1024, "{bytes}");
    assert!(messages.len() < 20, "the total limit dropped the old ones");
    assert_eq!(answer["messages_omitted"], 20 - messages.len());
    let tail = messages.last().unwrap();
    assert_eq!(tail["id"], json!(last), "the newest stays");
    assert_eq!(tail["text"], "короткий хвост");
    assert_eq!(tail["truncated"], false);
    assert_eq!(tail["next_offset"], Value::Null);
    let long = &messages[0];
    assert_eq!(long["truncated"], true);
    assert_eq!(long["total_bytes"], 20_000);
    let cut = long["text"].as_str().unwrap();
    assert!(cut.len() <= 4096 && cut.ends_with('ю'), "{}", cut.len());
    assert_eq!(long["next_offset"], cut.len(), "where the next read starts");

    assert_eq!(answer["activity"]["state"], "working");
    assert_eq!(answer["activity"]["evidence"]["kind"], "working");
    assert_eq!(answer["processes"].as_array().unwrap().len(), 1);
    assert_eq!(answer["processes"][0]["cpu_permille"], 250);
    assert_eq!(answer["processes"][0]["rss_bytes"], 4096);
    assert_eq!(answer["open_requests"][0]["kind"], "question");
    assert_eq!(answer["open_requests"][0]["summary"], "Какой пароль?");

    let short = f
        .call(f.root, "sessions.inspect", json!({"session_id": target}))
        .await
        .unwrap();
    assert_eq!(
        short["messages"].as_array().unwrap().len(),
        5,
        "the default"
    );
    for bad in [json!({"session_id": target, "messages": 21}), json!({})] {
        assert_eq!(
            f.call(f.root, "sessions.inspect", bad)
                .await
                .unwrap_err()
                .code,
            Code::InvalidArguments
        );
    }
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_long_block_is_paged_to_its_end(pool: PgPool) {
    let f = Fx::new(pool).await;
    let target = f.conversation("цель", None, 0).await;
    executions::start(&f.pool, f.owner, target, Uuid::now_v7())
        .await
        .unwrap();
    let full = "ю".repeat(50_000);
    let id = block(&f, target, "text", &full).await;

    let mut text = String::new();
    let mut offset = 0;
    let mut pages = 0;
    loop {
        let answer = f
            .call(
                f.root,
                "sessions.inspect",
                json!({"session_id": target, "block_id": id, "offset": offset}),
            )
            .await
            .unwrap();
        let page = &answer["block"];
        assert_eq!(page["id"], json!(id));
        assert_eq!(page["block_kind"], "text");
        assert_eq!(page["offset"], offset);
        assert_eq!(page["total_bytes"], 100_000);
        assert_eq!(answer.get("messages"), None, "only the requested page");
        let chunk = page["text"].as_str().unwrap();
        assert!(
            !chunk.is_empty() && chunk.len() <= 32 * 1024,
            "{}",
            chunk.len()
        );
        text.push_str(chunk);
        pages += 1;
        match page["next_offset"].as_i64() {
            Some(next) => offset = next,
            None => break,
        }
    }
    assert_eq!(text, full);
    assert_eq!(pages, 4);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn block_page_stays_within_what_inspect_shows(pool: PgPool) {
    let f = Fx::new(pool).await;
    let family = family(&f).await;
    for id in [family.child, family.stranger] {
        executions::start(&f.pool, f.owner, id, Uuid::now_v7())
            .await
            .unwrap();
    }
    let reasoning = block(&f, family.child, "reasoning", "скрытые рассуждения").await;
    let text = block(&f, family.child, "text", "ютекст").await;
    let foreign = block(&f, family.stranger, "text", "чужой текст").await;

    let refused = [
        json!({"session_id": family.child, "block_id": reasoning}),
        json!({"session_id": family.child, "block_id": foreign}),
        json!({"session_id": family.child, "block_id": Uuid::now_v7()}),
        json!({"session_id": family.child, "block_id": text, "offset": 1}),
        json!({"session_id": family.child, "block_id": text, "offset": 1000}),
        json!({"session_id": family.child, "block_id": text, "offset": -1}),
        json!({"session_id": family.child, "block_id": text, "messages": 1}),
        json!({"session_id": family.child, "offset": 0}),
    ];
    for args in refused {
        assert_eq!(
            f.call(f.root, "sessions.inspect", args.clone())
                .await
                .unwrap_err()
                .code,
            Code::InvalidArguments,
            "{args}"
        );
    }
    let hidden = f
        .call(
            family.child,
            "sessions.inspect",
            json!({"session_id": family.stranger, "block_id": foreign}),
        )
        .await
        .unwrap_err();
    assert_eq!(
        hidden.code,
        Code::SessionNotFound,
        "family rule still holds"
    );
    let whole = f
        .call(
            f.root,
            "sessions.inspect",
            json!({"session_id": family.child, "block_id": text}),
        )
        .await
        .unwrap();
    assert_eq!(whole["block"]["text"], "ютекст");
    assert_eq!(whole["block"]["offset"], 0);
    assert_eq!(whole["block"]["next_offset"], Value::Null);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn inspect_shows_the_last_run_and_worktree(pool: PgPool) {
    let f = Fx::new(pool).await;
    let target = f.conversation("цель", None, 0).await;
    let execution = executions::start(&f.pool, f.owner, target, Uuid::now_v7())
        .await
        .unwrap();
    sqlx::query("update conversations set worktree=$2 where id=$1")
        .bind(target)
        .bind(
            json!({"source_workdir":"/src","path":"/data/worktrees/x","branch":"canterlot/x",
                     "base":"HEAD","state":"active","detail":null,"branch_kept":null}),
        )
        .execute(&f.pool)
        .await
        .unwrap();
    let command = Uuid::now_v7();
    sqlx::query(
        "insert into commands(id,machine_id,conversation_id,execution_id,execution_generation,kind,payload,delivery_state)
         values($1,$2,$3,$4,$5,'deliver_input','{}','completed')",
    )
    .bind(command)
    .bind(f.machine)
    .bind(target)
    .bind(execution.execution_id)
    .bind(execution.generation)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into runs(input_command_id,execution_id,conversation_id,started_at,finished_at,duration_ms,git)
         values($1,$2,$3,now(),now(),1500,$4)",
    )
    .bind(command)
    .bind(execution.execution_id)
    .bind(target)
    .bind(json!({"available":true,"head_before":"aaa","head_after":"bbb","dirty":true,"truncated":false,
                 "commits":[{"sha":"bbb","subject":"тема"}],"files":[{"path":"a.rs","status":"M"}]}))
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query(
        "insert into run_model_usage(input_command_id,model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens)
         values($1,'small','main',12,3,0,0)",
    )
    .bind(command)
    .execute(&f.pool)
    .await
    .unwrap();
    let answer = f
        .call(f.root, "sessions.inspect", json!({"session_id": target}))
        .await
        .unwrap();
    assert_eq!(answer["session"]["title"], "цель");
    assert_eq!(answer["session"]["is_self"], false);
    assert_eq!(answer["last_run"]["duration_ms"], 1500);
    assert_eq!(answer["last_run"]["input_tokens"], 12);
    assert_eq!(answer["last_run"]["output_tokens"], 3);
    assert_eq!(answer["last_run"]["git"]["head_after"], "bbb");
    assert_eq!(answer["last_run"]["git"]["commits"], 1);
    assert_eq!(answer["last_run"]["git"]["files"], 1);
    assert_eq!(answer["worktree"]["branch"], "canterlot/x");
    assert_eq!(answer["worktree"]["state"], "active");
    assert_eq!(answer["worktree"]["path"], "/data/worktrees/x");
    assert_eq!(answer["worktree"].get("source_workdir"), None);

    let listed = f.call(f.root, "sessions.list", json!({})).await.unwrap();
    let row = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == json!(target))
        .unwrap();
    assert_eq!(row["worktree"]["branch"], "canterlot/x");
}
