#[path = "support/task7.rs"]
mod support;
use axum::http::Method;
use canterlot_hub::{history, testkit};
use serde_json::{Value, json};
use sqlx::PgPool;
use support::*;
use uuid::Uuid;

async fn timeline(f: &Fixture) -> Vec<i64> {
    let e = f.execution().await;
    let mut wanted = Vec::new();
    for n in 1..=12 {
        let i = f.input(&format!("i{n}")).await;
        let feed = n * 3;
        sqlx::query("update inputs set feed_seq=$2,state='accepted' where id=$1")
            .bind(i.input_id)
            .bind(feed)
            .execute(&f.pool)
            .await
            .unwrap();
        f.block(e, feed + 1, "text", "answer").await;
        wanted.extend([feed, feed + 1]);
    }
    wanted
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn history_pages_by_seq_without_gaps_or_duplicates(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let wanted = timeline(&f).await;
    let mut before = None;
    let mut got = Vec::new();
    loop {
        let p = f
            .get(&f.path(&format!(
                "/history?limit=5{}",
                before.map(|n| format!("&before={n}")).unwrap_or_default()
            )))
            .await;
        let mut k = keys(&p);
        k.extend(got);
        got = k;
        before = p["next_before"].as_str().map(str::to_owned);
        if before.is_none() {
            break;
        }
    }
    assert_eq!(got, wanted);
    assert!(
        f.client
            .send(
                Method::GET,
                &f.path("/history?before=1&after=2"),
                None,
                None
            )
            .await
            .status
            .is_client_error()
    );
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn history_after_pages_forward_without_gaps(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let wanted = timeline(&f).await;
    let mut after = "0".to_owned();
    let mut got = Vec::new();
    loop {
        let p = f
            .get(&f.path(&format!("/history?after={after}&limit=5")))
            .await;
        got.extend(keys(&p));
        let Some(n) = p["next_after"].as_str() else {
            break;
        };
        after = n.into();
    }
    assert_eq!(got, wanted);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn input_queued_during_a_turn_appears_after_that_turn_in_history(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    let m1 = f.input("m1").await;
    let m2 = f.input("m2").await;
    sqlx::query("update inputs set created_seq=case when id=$1 then 9 else 30 end,feed_seq=case when id=$1 then 10 else null end where conversation_id=$2").bind(m1.input_id).bind(f.seed.conversation_id).execute(&f.pool).await.unwrap();
    for n in (11..=25).chain([40]) {
        f.block(e, n, "text", "answer").await;
    }
    let p = f.get(&f.path("/history")).await;
    assert_eq!(keys(&p), (10..=25).chain([40]).collect::<Vec<_>>());
    assert!(
        !p["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == m2.input_id.to_string())
    );
    sqlx::query("insert into scope_counters(scope,last_seq) values($1,40) on conflict(scope) do update set last_seq=40").bind(format!("conversation:{}",f.seed.conversation_id)).execute(&f.pool).await.unwrap();
    sqlx::query("update inputs set state='dispatched' where id=$1")
        .bind(m2.input_id)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query(
        "update commands set execution_id=$2,execution_generation=1,dispatch_attempt=1 where id=$1",
    )
    .bind(m2.command_id)
    .bind(e)
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query("update queue_state set run_slot='running',run_slot_input_id=$2,hold_reasons='[]' where conversation_id=$1").bind(f.seed.conversation_id).bind(m2.input_id).execute(&f.pool).await.unwrap();
    canterlot_hub::delivery::receipts::apply_input_receipt(
        &f.pool,
        m2.command_id,
        1,
        canterlot_hub::delivery::receipts::InputReceipt::TransmissionStarted,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let feed: i64 = sqlx::query_scalar("select feed_seq from inputs where id=$1")
        .bind(m2.input_id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
    let event_seq:i64=sqlx::query_scalar("select scope_seq from events where kind='input_state_changed' and payload->>'input_id'=$1 and payload->>'state'='transmitting'").bind(m2.input_id.to_string()).fetch_one(&f.pool).await.unwrap();
    assert_eq!(feed, 41);
    assert_eq!(feed, event_seq);
    let p = f.get(&f.path("/history")).await;
    assert_eq!(keys(&p), (10..=25).chain([40, 41]).collect::<Vec<_>>());
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn first_history_page_reads_via_indexes_only(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    sqlx::query("insert into blocks(block_id,execution_id,conversation_id,kind,state,rev,created_seq) select gen_random_uuid(),$1,$2,'text','final',1,n from generate_series(1,10000) n").bind(e).bind(f.seed.conversation_id).execute(&f.pool).await.unwrap();
    sqlx::query("insert into commands(id,machine_id,conversation_id,kind,payload,delivery_state) select gen_random_uuid(),$1,$2,'deliver_input','{}','completed' from generate_series(1,10000)").bind(f.seed.machine_id).bind(f.seed.conversation_id).execute(&f.pool).await.unwrap();
    sqlx::query("insert into inputs(id,conversation_id,owner_id,input_seq,idempotency_key,content,state,command_id,received_at,feed_seq,created_seq) select gen_random_uuid(),$1,$2,row_number() over(),id::text,'[]','accepted',id,now(),10000+row_number() over(),10000+row_number() over() from commands").bind(f.seed.conversation_id).bind(f.seed.owner_id).execute(&f.pool).await.unwrap();
    sqlx::query("analyze blocks")
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("analyze inputs")
        .execute(&f.pool)
        .await
        .unwrap();
    let plan: Value = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "explain (format json) {}",
        history::TIMELINE_KEYS_SQL
    )))
    .bind(f.seed.conversation_id)
    .bind(0i64)
    .bind(false)
    .bind(false)
    .bind(101i64)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let text = plan.to_string();
    assert!(!text.contains("Seq Scan"));
    assert!(text.contains("blocks_timeline"));
    assert!(text.contains("inputs_feed"));
    assert_eq!(
        f.get(&f.path("/history")).await["items"]
            .as_array()
            .unwrap()
            .len(),
        100
    );
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn long_tool_output_is_truncated_in_history_and_paged_by_the_block_endpoint(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    let text = "line\n".repeat(70000);
    let id = f.block(e, 1, "tool_result", &text).await;
    let p = f.get(&f.path("/history")).await;
    let b = &p["items"][0];
    assert_eq!(b["content"].as_str().unwrap().len(), 4096);
    assert_eq!(b["content_truncated"], true);
    assert_eq!(b["content_bytes"], text.len());
    assert_eq!(b["line_count"], 70000);
    let mut offset = 0;
    let mut got = String::new();
    loop {
        let p = f
            .get(&f.path(&format!("/blocks/{id}?offset={offset}&limit=262144")))
            .await;
        assert!(p["content"].as_str().unwrap().len() <= 262144);
        got.push_str(p["content"].as_str().unwrap());
        let Some(n) = p["next_offset"].as_i64() else {
            break;
        };
        offset = n;
    }
    assert_eq!(got.len(), text.len());
    assert_eq!(
        canterlot_hub::blocks::checksum(&got),
        canterlot_hub::blocks::checksum(&text)
    );
    sqlx::query("update blocks set state='streaming',content='wrong' where block_id=$1")
        .bind(id)
        .execute(&f.pool)
        .await
        .unwrap();
    for (n, t) in [(2, "second"), (1, "first")] {
        sqlx::query("insert into block_chunks values($1,$2,$3)")
            .bind(id)
            .bind(n as i64)
            .bind(t)
            .execute(&f.pool)
            .await
            .unwrap();
    }
    assert_eq!(
        f.get(&f.path(&format!("/blocks/{id}"))).await["content"],
        "firstsecond"
    );
    assert_eq!(
        f.get(&f.path("/history")).await["items"][0]["content"],
        "firstsecond"
    );
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn block_offsets_are_utf8_bytes_on_char_boundaries(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    let text = "Я😀中文я";
    let id = f.block(e, 1, "text", text).await;
    for offset in [-1, 1, 3, 100] {
        let r = f
            .client
            .send(
                Method::GET,
                &f.path(&format!("/blocks/{id}?offset={offset}")),
                None,
                None,
            )
            .await;
        assert_eq!(r.status.as_u16(), 400);
        assert_eq!(r.json::<Value>()["error"]["code"], "invalid_offset");
    }
    for limit in [1, 2, 3, 4, 5, 7] {
        let mut offset = 0;
        let mut got = String::new();
        loop {
            let p = f
                .get(&f.path(&format!("/blocks/{id}?offset={offset}&limit={limit}")))
                .await;
            let s = p["content"].as_str().unwrap();
            assert!(text.is_char_boundary(offset as usize + s.len()));
            assert!(s.len() <= limit.max(4) as usize);
            got.push_str(s);
            let Some(n) = p["next_offset"].as_i64() else {
                break;
            };
            assert!(n > offset);
            offset = n;
        }
        assert_eq!(got, text);
    }
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn timeline_items_carry_created_at_and_tool_blocks_started_and_finished_at(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    let i = f.input("time").await;
    sqlx::query("update inputs set feed_seq=1 where id=$1")
        .bind(i.input_id)
        .execute(&f.pool)
        .await
        .unwrap();
    let id = f.block(e, 2, "tool_call", "x").await;
    sqlx::query(
        "update blocks set revision_gap=true,incomplete_reason='revision_gap' where block_id=$1",
    )
    .bind(id)
    .execute(&f.pool)
    .await
    .unwrap();
    let p = f.get(&f.path("/history")).await;
    for i in p["items"].as_array().unwrap() {
        assert!(i["created_at"].as_str().unwrap().ends_with('Z'));
    }
    let b = &p["items"][1];
    assert_eq!(b["started_at"], b["created_at"]);
    assert!(b["finished_at"].is_string());
    assert_eq!(b["revision_gap"], true);
    assert_eq!(b["incomplete_reason"], "revision_gap");
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn foreign_conversation_reads_are_not_found(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let other = testkit::seed(&f.pool).await;
    for id in [other.conversation_id, Uuid::now_v7()] {
        for suffix in [
            "/snapshot".into(),
            "/history".into(),
            "/delegation".into(),
            format!("/lanes/{}", Uuid::now_v7()),
            format!("/blocks/{}", Uuid::now_v7()),
            format!("/runs/{}", Uuid::now_v7()),
        ] {
            let r = f
                .client
                .send(
                    Method::GET,
                    &format!("/api/v1/conversations/{id}{suffix}"),
                    None,
                    None,
                )
                .await;
            assert_eq!(r.status.as_u16(), 404);
            assert_eq!(r.json::<Value>()["error"]["code"], "not_found");
        }
        let r = f
            .client
            .send(
                Method::GET,
                &format!("/api/v1/events?scope=conversation:{id}&after=0"),
                None,
                None,
            )
            .await;
        assert_eq!(r.status.as_u16(), 404);
    }
}
async fn run(f: &Fixture) -> (Uuid, Uuid, Uuid) {
    let e = f.execution().await;
    let i = f.input("run").await;
    sqlx::query("insert into runs(input_command_id,execution_id,conversation_id,provider,finished_at,duration_ms,context_tokens,context_window_tokens,weekly_before_permille,weekly_after_permille) values($1,$2,$3,'claude',clock_timestamp(),1000,123,200000,100,104)").bind(i.command_id).bind(e).bind(f.seed.conversation_id).execute(&f.pool).await.unwrap();
    sqlx::query("insert into run_model_usage values($1,'small','Explore',10,20,3,4)")
        .bind(i.command_id)
        .execute(&f.pool)
        .await
        .unwrap();
    let b = f.block(e, 2, "text", "x").await;
    sqlx::query("update blocks set input_command_id=$2 where block_id=$1")
        .bind(b)
        .bind(i.command_id)
        .execute(&f.pool)
        .await
        .unwrap();
    (i.command_id, i.input_id, b)
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_id_equals_input_command_id(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let (cmd, i, b) = run(&f).await;
    let r = f.get(&f.path(&format!("/runs/{cmd}"))).await;
    assert_eq!(r["run_id"], cmd.to_string());
    assert_eq!(r["input_id"], i.to_string());
    assert_eq!(
        f.get(&f.path("/history")).await["items"][0]["input_command_id"],
        cmd.to_string()
    );
    assert_eq!(r["parts"][0]["block_id"], b.to_string());
    assert_eq!(
        f.client
            .send(Method::GET, &f.path(&format!("/runs/{i}")), None, None)
            .await
            .status
            .as_u16(),
        404
    );
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn run_info_lists_usage_parts_and_overlap_note(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let (cmd, _, _) = run(&f).await;
    let p = f.path(&format!("/runs/{cmd}"));
    let r = f.get(&p).await;
    assert_eq!(r["duration_ms"], 1000);
    assert_eq!(r["context"]["tokens"], 123);
    assert_eq!(r["usage"][0]["output_tokens"], 20);
    assert_eq!(r["limit"]["weekly_delta_permille"], 4);
    assert_eq!(r["limit"]["exact"], true);
    assert!(r["compactions"].is_null());
    for clause in [
        "overlapped=true",
        "overlapped=false,exact=false",
        "exact=true,weekly_after_permille=100",
        "exact=true,weekly_after_permille=110",
    ] {
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "update runs set {clause} where input_command_id=$1"
        )))
        .bind(cmd)
        .execute(&f.pool)
        .await
        .unwrap();
        assert_eq!(f.get(&p).await["limit"]["exact"], false);
    }
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delegation_tree_lists_subagent_calls_with_state_name_model_and_task(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    let parent = f.block(e, 1, "tool_call", "args").await;
    let child = f.block(e, 2, "text", "answer").await;
    let later = f.block(e, 3, "text", "answer").await;
    sqlx::query("update blocks set agent='Main',model='main-model' where block_id=$1")
        .bind(parent)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("update blocks set parent_block_id=$2 where block_id=any($1)")
        .bind(vec![child, later])
        .bind(parent)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("update blocks set agent='Later',model='large' where block_id=$1")
        .bind(later)
        .execute(&f.pool)
        .await
        .unwrap();
    let p = f.path("/delegation");
    let r = f.get(&p).await;
    assert_eq!(r.as_array().unwrap().len(), 1);
    assert_eq!(r[0]["block_id"], parent.to_string());
    assert_eq!(r[0]["subagent"], json!({"name":"Explore","model":"small"}));
    assert_eq!(r[0]["task"], "task");
    assert_eq!(r[0]["state"], "done");
    for (stored, wire) in [("streaming", "running"), ("incomplete", "incomplete")] {
        sqlx::query("update blocks set state=$2 where block_id=$1")
            .bind(parent)
            .bind(stored)
            .execute(&f.pool)
            .await
            .unwrap();
        assert_eq!(f.get(&p).await[0]["state"], wire);
    }
}
/// Фоновый агент: первым ребёнком вызова идёт результат запуска основного агента,
/// настоящее имя и модель субагента приходят с его собственными блоками позже.
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn delegation_names_the_subagent_not_the_launch_result(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    let call = f.block(e, 1, "tool_call", "args").await;
    let launch = f.block(e, 2, "tool_result", "launched").await;
    let inner = f.block(e, 3, "tool_call", "ls").await;
    let later = f.block(e, 4, "text", "answer").await;
    sqlx::query("update blocks set agent='orchestrator',model='opus-main' where block_id=any($1)")
        .bind(vec![call, launch])
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query("update blocks set summary='async_launched' where block_id=$1")
        .bind(launch)
        .execute(&f.pool)
        .await
        .unwrap();
    sqlx::query(
        "update blocks set agent='general-purpose',model='sonnet-sub' where block_id=any($1)",
    )
    .bind(vec![inner, later])
    .execute(&f.pool)
    .await
    .unwrap();
    sqlx::query("update blocks set parent_block_id=$2 where block_id=any($1)")
        .bind(vec![launch, inner, later])
        .bind(call)
        .execute(&f.pool)
        .await
        .unwrap();
    let r = f.get(&f.path("/delegation")).await;
    assert_eq!(
        r[0]["subagent"],
        json!({"name":"general-purpose","model":"sonnet-sub"})
    );
    sqlx::query("delete from blocks where block_id=any($1)")
        .bind(vec![inner, later])
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(f.get(&f.path("/delegation")).await[0]["subagent"].is_null());
}
async fn adopt(f: &Fixture, parent: Uuid, children: &[Uuid]) {
    sqlx::query("update blocks set parent_block_id=$2 where block_id=any($1)")
        .bind(children)
        .bind(parent)
        .execute(&f.pool)
        .await
        .unwrap();
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lane_pages_the_whole_subtree_newest_first_without_gaps(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    let call = f.block(e, 1, "tool_call", "agent").await;
    let other = f.block(e, 2, "tool_call", "other agent").await;
    let mut lane = Vec::new();
    // Ветка старше окна ленты: шаги, их результаты и вложенный субагент с потомком.
    for n in 0..7 {
        let step = f.block(e, 10 + n * 10, "tool_call", "step").await;
        let result = f.block(e, 11 + n * 10, "tool_result", "out").await;
        adopt(&f, call, &[step]).await;
        adopt(&f, step, &[result]).await;
        lane.extend([step, result]);
    }
    let nested = f.block(e, 100, "tool_call", "nested").await;
    let deep = f.block(e, 101, "text", "deep").await;
    adopt(&f, call, &[nested]).await;
    adopt(&f, nested, &[deep]).await;
    lane.extend([nested, deep]);
    let alien = f.block(e, 50, "text", "other branch").await;
    adopt(&f, other, &[alien]).await;
    let mut before = None::<String>;
    let mut got: Vec<Uuid> = Vec::new();
    let mut pages = 0;
    loop {
        let p = f
            .get(&f.path(&format!(
                "/lanes/{call}?limit=4{}",
                before
                    .as_ref()
                    .map(|n| format!("&before={n}"))
                    .unwrap_or_default()
            )))
            .await;
        assert_eq!(p["root"]["id"], call.to_string());
        assert!(p["items"].as_array().unwrap().len() <= 4);
        let mut ids: Vec<Uuid> = p["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().parse().unwrap())
            .collect();
        ids.extend(got);
        got = ids;
        pages += 1;
        before = p["next_before"].as_str().map(str::to_owned);
        if before.is_none() {
            break;
        }
    }
    assert_eq!(pages, 4);
    assert_eq!(
        got, lane,
        "ascending order, nothing foreign, nothing missing"
    );
    assert!(!got.contains(&call) && !got.contains(&alien));
    let tail = f.get(&f.path(&format!("/lanes/{call}"))).await;
    assert_eq!(tail["items"].as_array().unwrap().len(), 16);
    assert!(tail["next_before"].is_null());
    assert_eq!(tail["items"][15]["id"], deep.to_string());
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn lane_of_a_leaf_is_just_the_root_and_bad_requests_are_rejected(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    let leaf = f.block(e, 1, "tool_call", "leaf").await;
    let p = f.get(&f.path(&format!("/lanes/{leaf}"))).await;
    assert_eq!(p["root"]["id"], leaf.to_string());
    assert_eq!(p["items"], json!([]));
    assert!(p["next_before"].is_null());
    for query in [
        "limit=0",
        "limit=201",
        "before=x",
        "before=1&after=2",
        "x=1",
    ] {
        let r = f
            .client
            .send(
                Method::GET,
                &f.path(&format!("/lanes/{leaf}?{query}")),
                None,
                None,
            )
            .await;
        assert_eq!(r.status.as_u16(), 400, "{query}");
    }
    let r = f
        .client
        .send(
            Method::GET,
            &f.path(&format!("/lanes/{}", Uuid::now_v7())),
            None,
            None,
        )
        .await;
    assert_eq!(r.status.as_u16(), 404);
}
#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn input_items_expose_the_idempotency_key_to_the_owner_only(pool: PgPool) {
    let f = Fixture::new(pool).await;
    let i = f.input("owner-key").await;
    sqlx::query("update inputs set feed_seq=1 where id=$1")
        .bind(i.input_id)
        .execute(&f.pool)
        .await
        .unwrap();
    let p = f.get(&f.path("/history")).await;
    assert_eq!(p["items"][0]["idempotency_key"], "owner-key");
    assert!(p["items"][0]["created_seq"].is_string());
    assert_eq!(p["items"][0]["feed_seq"], "1");
    let mut conn = f.pool.acquire().await.unwrap();
    assert!(
        history::page(
            &mut conn,
            Uuid::now_v7(),
            f.seed.conversation_id,
            history::HistoryCursor::Tail,
            100
        )
        .await
        .unwrap()
        .is_none()
    );
}

#[sqlx::test(migrations = false)]
async fn legacy_input_and_block_keys_do_not_collide_after_migration(pool: PgPool) {
    let old = sqlx::migrate::Migrator {
        migrations: canterlot_hub::db::MIGRATOR
            .iter()
            // Всё, кроме проверяемой 0012: текущий код пишет старые строки до её применения.
            .filter(|m| m.version != 12)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    old.run(&pool).await.unwrap();
    let f = Fixture::new(pool).await;
    let e = f.execution().await;
    let i = f.input("legacy").await;
    sqlx::query("update inputs set state='accepted',created_seq=-1,feed_seq=-1 where id=$1")
        .bind(i.input_id)
        .execute(&f.pool)
        .await
        .unwrap();
    let b = f.block(e, -1, "text", "legacy").await;
    canterlot_hub::db::MIGRATOR.run(&f.pool).await.unwrap();
    let mut conn = f.pool.acquire().await.unwrap();
    let mut cursor = history::HistoryCursor::Tail;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let p = history::page(
            &mut conn,
            f.seed.owner_id,
            f.seed.conversation_id,
            cursor,
            1,
        )
        .await
        .unwrap()
        .unwrap();
        let p = serde_json::to_value(p).unwrap();
        for item in p["items"].as_array().unwrap() {
            assert!(seen.insert(item["id"].as_str().unwrap().parse::<Uuid>().unwrap()));
        }
        let Some(before) = p["next_before"].as_str() else {
            break;
        };
        cursor = history::HistoryCursor::Before(before.parse().unwrap());
    }
    assert_eq!(seen, std::collections::BTreeSet::from([i.input_id, b]));
}
