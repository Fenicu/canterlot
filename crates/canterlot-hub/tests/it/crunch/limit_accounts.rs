//! Лимиты по учётным записям: у каждого харнесса машины своя метка `account`, и состояние
//! лимитов, удержания, пробуждения, опрос, занятость и допуск идут по паре
//! (провайдер, учётная запись). Метка `owner` оставляет ключи и JSON прежними.

use super::*;
use canterlot_hub::machine_info;
use canterlot_hub::session_tools::routing;
use canterlot_proto::worker::v1::{HarnessInfo, MachineReport};

const B: &str = "b-acct";

/// Машина того же провайдера (`claude`) с другой учётной записью, подключённая к тому же реестру.
async fn add_account_machine(env: &mut Env, account: &str) -> Uuid {
    let id = new_machine(&env.pool, env.owner, "claude").await;
    sqlx::query("update machine_harnesses set account=$2 where machine_id=$1")
        .bind(id)
        .bind(account)
        .execute(&env.pool)
        .await
        .unwrap();
    let FakeSession { commands, lease } =
        testkit::fake_session(&env.registry, id, &["pilot_v1", "instructions_v1"], true);
    env.receivers.insert(id, commands);
    env._leases.push(lease);
    id
}

/// Показание, снятое машиной `machine` для учётной записи `account`.
async fn observe_as(
    env: &Env,
    machine: Uuid,
    account: &str,
    window: Window,
    used: u16,
    resets_at: Option<DateTime<Utc>>,
    at: DateTime<Utc>,
) {
    let mut tx = env.pool.begin().await.unwrap();
    limits::record_observation_in_tx(
        &mut tx,
        env.owner,
        &Observation {
            provider: "claude".into(),
            account: account.into(),
            window,
            used_permille: used,
            resets_at,
            observed_at: at,
            source: ObservationSource::Session,
            data_confidence: DataConfidence::Exact,
            machine_id: machine,
            execution_id: None,
        },
        at,
        &env.config,
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

fn weekly_of(holds: &[HoldReason], account: &str) -> bool {
    holds.iter().any(
        |h| matches!(h, HoldReason::WeeklyReserve { provider, account: a, .. } if provider == "claude" && a == account),
    )
}

fn quota_of(holds: &[HoldReason], account: &str) -> bool {
    holds.iter().any(
        |h| matches!(h, HoldReason::QuotaExhausted { provider, account: a, .. } if provider == "claude" && a == account),
    )
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn observations_of_two_accounts_land_in_separate_rows(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let b = add_account_machine(&mut env, B).await;
    let now = start();
    let reset = now + TimeDelta::hours(3);
    env.observe("claude", Window::Weekly, 300, Some(reset), now)
        .await;
    observe_as(&env, b, B, Window::Weekly, 800, Some(reset), now).await;
    let rows: Vec<(String, i32)> = sqlx::query_as(
        "select account,weekly_used_permille from provider_limits where provider='claude' order by account",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows, vec![(B.to_owned(), 800), ("owner".to_owned(), 300)]);
    let snapshots: Vec<(String, i32)> = sqlx::query_as(
        "select account,used_permille from limit_snapshots where provider='claude' order by account",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        snapshots,
        vec![(B.to_owned(), 800), ("owner".to_owned(), 300)]
    );
    // Ряд истории и показ видят каждую учётную запись отдельно.
    let owner_series = limits::series(
        &pool,
        "claude",
        "owner",
        Window::Weekly,
        now - secs(60),
        now + secs(60),
    )
    .await
    .unwrap();
    let b_series = limits::series(
        &pool,
        "claude",
        B,
        Window::Weekly,
        now - secs(60),
        now + secs(60),
    )
    .await
    .unwrap();
    assert_eq!(
        owner_series.iter().map(|p| p.1).collect::<Vec<_>>(),
        vec![300]
    );
    assert_eq!(b_series.iter().map(|p| p.1).collect::<Vec<_>>(), vec![800]);
    let mut conn = pool.acquire().await.unwrap();
    let shown = limits::shown(&mut conn, env.owner).await.unwrap();
    let pairs: Vec<(String, String)> = shown
        .iter()
        .filter(|row| row.provider == "claude")
        .map(|row| (row.provider.clone(), row.account.clone()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("claude".to_owned(), B.to_owned()),
            ("claude".to_owned(), "owner".to_owned())
        ]
    );
    // События несут свою учётную запись.
    let accounts: Vec<String> = sqlx::query_scalar(
        "select payload->>'account' from events where kind='limits_changed' order by id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(accounts.contains(&"owner".to_owned()) && accounts.contains(&B.to_owned()));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_reading_of_an_account_arriving_before_its_machine_report_lands_in_that_account(
    pool: PgPool,
) {
    let env = Env::new(&pool).await;
    // Машина без строк каталога: отчёт машины ещё не приходил.
    let silent = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'late')")
        .bind(silent)
        .bind(env.owner)
        .execute(&pool)
        .await
        .unwrap();
    let now = start();
    observe_as(
        &env,
        silent,
        B,
        Window::Weekly,
        640,
        Some(now + secs(3600)),
        now,
    )
    .await;
    let stored: i32 = sqlx::query_scalar(
        "select weekly_used_permille from provider_limits where provider='claude' and account=$1",
    )
    .bind(B)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, 640);
    let owner_rows: i64 = sqlx::query_scalar(
        "select count(*) from provider_limits where provider='claude' and account='owner'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(owner_rows, 0, "the reading never touches the owner account");
    // Показ строки ждёт отчёта машины, который предлагает эту учётную запись.
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        limits::shown(&mut conn, env.owner)
            .await
            .unwrap()
            .iter()
            .all(|row| row.account != B)
    );
    sqlx::query("insert into machine_harnesses(machine_id,harness,provider,title,account) values($1,'synthetic','claude','t',$2)")
        .bind(silent)
        .bind(B)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        limits::shown(&mut conn, env.owner)
            .await
            .unwrap()
            .iter()
            .any(|row| row.account == B)
    );
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn an_unstamped_fact_from_a_machine_with_another_account_is_dropped(pool: PgPool) {
    let mut env = Env::new(&pool).await;
    let b = add_account_machine(&mut env, B).await;
    let bare = Uuid::now_v7();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'bare')")
        .bind(bare)
        .bind(env.owner)
        .execute(&pool)
        .await
        .unwrap();
    let resolve = |machine, provider: &'static str, stamped: &'static str| {
        let pool = pool.clone();
        async move {
            limits::fact_account(
                &mut pool.acquire().await.unwrap(),
                machine,
                provider,
                stamped,
            )
            .await
        }
    };
    // Старый воркер на машине владельца: факт без метки — это `owner`.
    assert_eq!(
        resolve(env.claude, "claude", "").await.unwrap().as_deref(),
        Some("owner")
    );
    // Машина объявила для провайдера другую учётную запись: факт без метки отбрасывается.
    assert_eq!(resolve(b, "claude", "").await.unwrap(), None);
    // Другой провайдер этой же машины не затронут.
    assert_eq!(
        resolve(b, "codex", "").await.unwrap().as_deref(),
        Some("owner")
    );
    // Метка воркера берётся как есть, даже у машины без строк каталога.
    assert_eq!(resolve(b, "claude", B).await.unwrap().as_deref(), Some(B));
    assert_eq!(
        resolve(bare, "claude", "").await.unwrap().as_deref(),
        Some("owner")
    );
    assert!(resolve(b, "claude", "Bad_Label").await.is_err());
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn holds_and_wakes_of_one_account_never_touch_a_conversation_of_another(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let b = add_account_machine(&mut env, B).await;
    let owner_conversation = env.conversation(true, true).await;
    let b_conversation = env.conversation_on(b, true, true).await;
    let now = start();
    let reset = now + secs(3600);
    env.limit_reached_as(
        owner_conversation,
        "claude",
        "owner",
        Window::FiveHour,
        Some(reset),
        now,
    )
    .await;
    env.limit_reached_as(
        b_conversation,
        "claude",
        B,
        Window::FiveHour,
        Some(reset),
        now,
    )
    .await;
    assert!(quota_of(&holds(&pool, owner_conversation).await, "owner"));
    assert!(!quota_of(&holds(&pool, owner_conversation).await, B));
    assert!(quota_of(&holds(&pool, b_conversation).await, B));
    assert!(!quota_of(&holds(&pool, b_conversation).await, "owner"));
    // Ключи: `owner` прежний, другая запись получает суффикс у провайдера.
    let wakes = pending(&pool, "wake").await;
    let key_of = |conversation: Uuid| {
        wakes
            .iter()
            .find(|job| job.conversation_id == Some(conversation))
            .map(|job| job.payload.clone())
            .expect("wake job")
    };
    let owner_payload = key_of(owner_conversation);
    assert_eq!(owner_payload["hold_key"], "quota:claude:five_hour");
    assert!(owner_payload.get("account").is_none());
    let b_payload = key_of(b_conversation);
    assert_eq!(b_payload["hold_key"], format!("quota:claude@{B}:five_hour"));
    assert_eq!(b_payload["account"], B);

    // Сброс одной учётной записи будит только её разговоры.
    observe_as(
        &env,
        b,
        B,
        Window::FiveHour,
        30,
        Some(reset + secs(5 * 3600)),
        reset + secs(30),
    )
    .await;
    env.tick(reset + secs(120)).await;
    assert!(holds(&pool, b_conversation).await.is_empty());
    assert_eq!(
        wake_texts(&pool, b_conversation).await,
        vec![LimitsConfig::default().default_resume_text]
    );
    assert!(quota_of(&holds(&pool, owner_conversation).await, "owner"));
    assert!(wake_texts(&pool, owner_conversation).await.is_empty());
    let still_pending = pending(&pool, "wake").await;
    assert_eq!(still_pending.len(), 1);
    assert_eq!(still_pending[0].conversation_id, Some(owner_conversation));
    // Опрос, который просит ожидание владельца, идёт машине его учётной записи.
    env.drain();
    let polled: Vec<(Uuid, String)> = env
        .received
        .iter()
        .filter_map(|(machine, message)| match &message.msg {
            Some(control_down::Msg::PollLimits(p)) => Some((*machine, p.account.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(polled, vec![(env.claude, "owner".to_owned())]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_weekly_reserve_of_one_account_holds_only_its_conversations(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let b = add_account_machine(&mut env, B).await;
    let owner_conversation = env.conversation(true, true).await;
    let b_conversation = env.conversation_on(b, true, true).await;
    let now = start();
    let weekly_reset = now + TimeDelta::days(3);
    observe_as(&env, b, B, Window::Weekly, 960, Some(weekly_reset), now).await;
    // Строки пар: резерв включён только у учётной записи B.
    let rows: Vec<(String, bool)> = sqlx::query_as(
        "select account,weekly_reserve_active from provider_limits where provider='claude' order by account",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows, vec![(B.to_owned(), true)]);
    // Задание правила несёт учётную запись; у `owner` ключа в нагрузке нет.
    let rules = pending(&pool, "limit_rule").await;
    assert_eq!(rules.len(), 1);
    assert_eq!(
        rules[0].payload,
        json!({"provider": "claude", "account": B})
    );
    // Резерв уже действует как выводимое удержание в шлюзе выдачи: только у записи B.
    env.submit(owner_conversation, "owner input").await;
    env.submit(b_conversation, "b input").await;
    let held = |outcome: DispatchOutcome, account: &str| match outcome {
        DispatchOutcome::Blocked(reasons) => reasons
            .iter()
            .any(|r| matches!(r, BlockReason::Held(h) if weekly_of(h, account))),
        _ => false,
    };
    let b_outcome = dispatch_next(&pool, b_conversation, gate(), Uuid::now_v7())
        .await
        .unwrap();
    assert!(held(b_outcome, B), "the account in reserve is held");
    let owner_outcome = dispatch_next(&pool, owner_conversation, gate(), Uuid::now_v7())
        .await
        .unwrap();
    assert!(
        !held(owner_outcome.clone(), "owner") && !held(owner_outcome, B),
        "the other account is not held by it"
    );
    // Правило ставит удержание на разговоры этой записи и расписывает проверку.
    env.tick(now).await;
    assert!(weekly_of(&holds(&pool, b_conversation).await, B));
    assert!(!weekly_of(&holds(&pool, owner_conversation).await, B));
    assert!(!weekly_of(&holds(&pool, owner_conversation).await, "owner"));
    let checks = pending(&pool, "limit_check").await;
    assert_eq!(checks.len(), 1);
    assert_eq!(
        checks[0].payload,
        json!({"provider": "claude", "account": B})
    );
    // Начальные удержания нового разговора считаются по учётной записи его харнесса.
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        crunch::initial_holds_in_tx(&mut conn, "claude", "owner")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(weekly_of(
        &crunch::initial_holds_in_tx(&mut conn, "claude", B)
            .await
            .unwrap(),
        B
    ));
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn a_label_change_releases_the_holds_of_the_old_account(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let b = add_account_machine(&mut env, B).await;
    let owner_conversation = env.conversation(true, true).await;
    let b_conversation = env.conversation_on(b, true, true).await;
    let now = start();
    let reset = now + secs(3600);
    env.limit_reached_as(
        owner_conversation,
        "claude",
        "owner",
        Window::FiveHour,
        Some(reset),
        now,
    )
    .await;
    env.limit_reached_as(
        b_conversation,
        "claude",
        B,
        Window::FiveHour,
        Some(reset),
        now,
    )
    .await;
    // Новая учётная запись уже в резерве: её состояние выводится заново.
    let weekly_reset = now + TimeDelta::days(2);
    sqlx::query("insert into provider_limits(provider,account,weekly_used_permille,weekly_resets_at,weekly_observed_at,weekly_reserve_active) values('claude','c-acct',990,$1,$2,true)")
        .bind(weekly_reset)
        .bind(now)
        .execute(&pool)
        .await
        .unwrap();
    let report = MachineReport {
        worker_version: "test".into(),
        allowed_roots: vec!["/tmp".into()],
        containment: "process_group".into(),
        os: "linux".into(),
        max_attachment_bytes: canterlot_proto::limits::MAX_ATTACHMENT_BYTES,
        max_attachments_per_input: canterlot_proto::limits::MAX_ATTACHMENTS_PER_INPUT as u32,
        harnesses: vec![HarnessInfo {
            harness: "synthetic".into(),
            provider: "claude".into(),
            title: "t".into(),
            account: "c-acct".into(),
            ..Default::default()
        }],
        ..Default::default()
    };
    let released = machine_info::apply_report_with_capabilities(
        &pool,
        env.owner,
        b,
        &report,
        &[],
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    assert_eq!(released, vec![b_conversation]);
    let after = holds(&pool, b_conversation).await;
    assert!(!quota_of(&after, B), "{after:?}");
    assert!(weekly_of(&after, "c-acct"), "{after:?}");
    // Ожидающее пробуждение старой записи отменено, чужая запись не тронута.
    let wakes = jobs(&pool, "wake").await;
    let b_wake = wakes
        .iter()
        .find(|job| job.conversation_id == Some(b_conversation))
        .unwrap();
    assert_eq!(b_wake.state, "cancelled");
    assert_eq!(b_wake.result.as_ref().unwrap()["reason"], "account_changed");
    assert!(quota_of(&holds(&pool, owner_conversation).await, "owner"));
    let owner_wake = wakes
        .iter()
        .find(|job| job.conversation_id == Some(owner_conversation))
        .unwrap();
    assert_eq!(owner_wake.state, "pending");
    let stored: String = sqlx::query_scalar(
        "select account from machine_harnesses where machine_id=$1 and harness='synthetic'",
    )
    .bind(b)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, "c-acct");
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn polls_and_snapshots_go_only_to_machines_of_the_account(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let b = add_account_machine(&mut env, B).await;
    let now = start();
    let five = Duration::from_secs(300);
    assert_eq!(
        crunch::request_poll_outcome(&pool, &env.registry, "claude", B, five, false, now)
            .await
            .unwrap(),
        crunch::PollRequest::Sent
    );
    env.drain();
    let polls: Vec<(Uuid, String, String)> = env
        .received
        .iter()
        .filter_map(|(machine, message)| match &message.msg {
            Some(control_down::Msg::PollLimits(p)) => {
                Some((*machine, p.provider.clone(), p.account.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(polls, vec![(b, "claude".to_owned(), B.to_owned())]);
    env.received.clear();
    // Троттлинг ведётся по паре: просьба по другой учётной записи не ждёт первой.
    assert_eq!(
        crunch::request_poll_outcome(&pool, &env.registry, "claude", "owner", five, false, now)
            .await
            .unwrap(),
        crunch::PollRequest::Sent
    );
    assert_eq!(
        crunch::request_poll_outcome(&pool, &env.registry, "claude", B, five, false, now)
            .await
            .unwrap(),
        crunch::PollRequest::Throttled
    );
    env.drain();
    let polls: Vec<(Uuid, String)> = env
        .received
        .iter()
        .filter_map(|(machine, message)| match &message.msg {
            Some(control_down::Msg::PollLimits(p)) => Some((*machine, p.account.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(polls, vec![(env.claude, "owner".to_owned())]);
    env.received.clear();
    // Снимок состояния идёт только машинам с этой учётной записью.
    observe_as(&env, b, B, Window::Weekly, 500, Some(now + secs(3600)), now).await;
    env.drain();
    env.received.clear();
    let sent = limits::push_limits_snapshot(&pool, &env.registry, "claude", B)
        .await
        .unwrap();
    assert_eq!(sent, 1);
    env.drain();
    let snapshots: Vec<(Uuid, String)> = env
        .received
        .iter()
        .filter_map(|(machine, message)| match &message.msg {
            Some(control_down::Msg::LimitsSnapshot(s)) => Some((*machine, s.account.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(snapshots, vec![(b, B.to_owned())]);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn max_parallel_busy_counts_are_per_account(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let b = add_account_machine(&mut env, B).await;
    let owner_conversation = env.conversation(true, true).await;
    let b_conversation = env.conversation_on(b, true, true).await;
    for conversation in [owner_conversation, b_conversation] {
        sqlx::query("update conversations set parent_conversation_id=$2 where id=$1")
            .bind(conversation)
            .bind(env.conversation(false, false).await)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("insert into conversation_derived(conversation_id,activity_state) values($1,'working') on conflict(conversation_id) do update set activity_state='working'")
            .bind(conversation)
            .execute(&pool)
            .await
            .unwrap();
    }
    let mut conn = pool.acquire().await.unwrap();
    let pair = ("synthetic".to_owned(), "synthetic-small".to_owned());
    // Потолок бережёт подписку: с машины владельца видна только сессия владельца...
    let from_owner = routing::live_counts(&mut conn, env.owner, env.claude)
        .await
        .unwrap();
    assert_eq!(from_owner.get(&pair), Some(&1));
    // ...и с машины B только сессия учётной записи B.
    let from_b = routing::live_counts(&mut conn, env.owner, b).await.unwrap();
    assert_eq!(from_b.get(&pair), Some(&1));
    // Две сессии одной учётной записи складываются.
    sqlx::query("update machine_harnesses set account='owner' where machine_id=$1")
        .bind(b)
        .execute(&pool)
        .await
        .unwrap();
    let merged = routing::live_counts(&mut conn, env.owner, env.claude)
        .await
        .unwrap();
    assert_eq!(merged.get(&pair), Some(&2));
}

#[test]
fn owner_hold_and_pause_json_stay_byte_identical_and_other_accounts_add_a_key() {
    // Значения, сохранённые до учётных записей, читаются как `owner` и пишутся без ключа.
    let stored_quota = json!({"reason":"quota_exhausted","provider":"claude","window":"five_hour","wake_at":"2026-11-27T10:00:00.000Z"});
    let stored_weekly = json!({"reason":"weekly_reserve","provider":"claude","resets_at":"2026-11-27T10:00:00.000Z"});
    let stored_pause = json!({"reason":"limit_reached","provider":"claude","window":"weekly","resets_at":"2026-11-27T10:00:00.000Z"});
    for stored in [&stored_quota, &stored_weekly] {
        let hold: HoldReason = serde_json::from_value(stored.clone()).unwrap();
        assert!(matches!(
            &hold,
            HoldReason::QuotaExhausted { account, .. } | HoldReason::WeeklyReserve { account, .. }
                if account == "owner"
        ));
        assert_eq!(&serde_json::to_value(&hold).unwrap(), stored);
    }
    let pause: PauseReason = serde_json::from_value(stored_pause.clone()).unwrap();
    assert!(matches!(&pause, PauseReason::LimitReached { account, .. } if account == "owner"));
    assert_eq!(serde_json::to_value(&pause).unwrap(), stored_pause);
    // Другая учётная запись добавляет ключ.
    let mut with_account = stored_weekly.clone();
    with_account["account"] = json!(B);
    let hold: HoldReason = serde_json::from_value(with_account.clone()).unwrap();
    assert_eq!(serde_json::to_value(&hold).unwrap(), with_account);
    // Вид для PWA несёт то же поле.
    let view = serde_json::to_value(canterlot_hub::model::HoldView::from(hold)).unwrap();
    assert_eq!(view["account"], B);
}

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn pending_jobs_and_holds_stored_before_accounts_survive_the_upgrade(pool: PgPool) {
    let mut env = Env::with_config(&pool, patient_config()).await;
    let waiting = env.conversation(true, true).await;
    let now = start();
    let reset = now + secs(3600);
    // Состояние, как его записала версия до учётных записей: ни `account` в JSON, ни в нагрузке.
    let legacy_hold = json!([{"reason":"quota_exhausted","provider":"claude","window":"five_hour","wake_at":reset.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)}]);
    sqlx::query("update queue_state set hold_reasons=$2 where conversation_id=$1")
        .bind(waiting)
        .bind(&legacy_hold)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("update conversations set resume_pending=true,resume_pending_until=$2 where id=$1")
        .bind(waiting)
        .bind(reset + TimeDelta::days(8))
        .execute(&pool)
        .await
        .unwrap();
    let legacy = |kind: &str, conversation: Option<Uuid>, payload: Value, at: DateTime<Utc>| {
        let pool = pool.clone();
        let owner = env.owner;
        let kind = kind.to_owned();
        async move {
            sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) values($1,$2,$3,$4,$5,$6)")
                .bind(Uuid::now_v7())
                .bind(kind)
                .bind(owner)
                .bind(conversation)
                .bind(at)
                .bind(payload)
                .execute(&pool)
                .await
                .unwrap();
        }
    };
    legacy(
        "wake",
        Some(waiting),
        json!({"provider":"claude","window":"five_hour","hold_key":"quota:claude:five_hour","resets_at":reset.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)}),
        reset + secs(20),
    )
    .await;
    // Проверка и правило в очереди до обновления: срок далеко, их тик не берёт.
    let far = now + TimeDelta::days(30);
    legacy("limit_check", None, json!({"provider":"claude"}), far).await;
    legacy("limit_rule", None, json!({"provider":"claude"}), far).await;
    // Старое пробуждение срабатывает: показание после срока подтверждает сброс.
    verified_reset(&env, reset).await;
    env.tick(reset + secs(120)).await;
    assert!(!quota_of(&holds(&pool, waiting).await, "owner"));
    assert_eq!(
        wake_texts(&pool, waiting).await,
        vec![LimitsConfig::default().default_resume_text]
    );
    // Резерв владельца ставит то же правило и ту же проверку: дубликатов нет.
    let weekly_reset = now + TimeDelta::days(3);
    env.observe(
        "claude",
        Window::Weekly,
        975,
        Some(weekly_reset),
        reset + secs(200),
    )
    .await;
    let rules = pending(&pool, "limit_rule").await;
    assert_eq!(rules.len(), 1);
    assert_eq!(
        rules[0].payload,
        json!({"provider":"claude"}),
        "the owner payload stays as it was"
    );
    let mut tx = pool.begin().await.unwrap();
    crunch::apply_weekly_rule_in_tx(
        &mut tx,
        env.owner,
        "claude",
        "owner",
        &env.config,
        reset + secs(210),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let checks = pending(&pool, "limit_check").await;
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].payload, json!({"provider":"claude"}));
    // Новое удержание владельца пишется теми же ключами, что и до обновления.
    let fresh = env.conversation(true, true).await;
    env.limit_reached(
        fresh,
        "claude",
        Window::FiveHour,
        Some(reset + secs(7200)),
        now,
    )
    .await;
    let key = pending(&pool, "wake")
        .await
        .into_iter()
        .find(|job| job.conversation_id == Some(fresh))
        .unwrap();
    assert_eq!(key.payload["hold_key"], "quota:claude:five_hour");
    assert!(key.payload.get("account").is_none());
    let stored: Value =
        sqlx::query_scalar("select hold_reasons->0 from queue_state where conversation_id=$1")
            .bind(fresh)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(stored.get("account").is_none(), "{stored}");
}

#[sqlx::test(migrations = false)]
async fn migration_0122_gives_existing_rows_the_owner_account_and_keeps_pending_jobs(pool: PgPool) {
    canterlot_hub::db::MIGRATOR
        .run_to(121, &pool)
        .await
        .unwrap();
    let owner = Uuid::now_v7();
    let machine = Uuid::now_v7();
    sqlx::query("insert into owners(id) values($1)")
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into machines(id,owner_id,name) values($1,$2,'m')")
        .bind(machine)
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into machine_harnesses(machine_id,harness,provider,title) values($1,'synthetic','claude','t')")
        .bind(machine)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into provider_limits(provider,weekly_used_permille,weekly_reserve_active) values('claude',970,true)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into limit_snapshots(provider,window_kind,used_permille,observed_at,source,machine_id) values('claude','weekly',970,now(),'poll',$1)")
        .bind(machine)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("insert into limit_polls(provider,last_requested_at) values('claude',now())")
        .execute(&pool)
        .await
        .unwrap();
    for kind in ["limit_check", "limit_rule"] {
        sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) values($1,$2,$3,null,now(),'{\"provider\":\"claude\"}')")
            .bind(Uuid::now_v7())
            .bind(kind)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
    }
    canterlot_hub::db::MIGRATOR.run(&pool).await.unwrap();
    for (table, query) in [
        (
            "provider_limits",
            "select distinct account from provider_limits",
        ),
        (
            "limit_snapshots",
            "select distinct account from limit_snapshots",
        ),
        ("limit_polls", "select distinct account from limit_polls"),
        (
            "machine_harnesses",
            "select distinct account from machine_harnesses",
        ),
    ] {
        let accounts: Vec<String> = sqlx::query_scalar(query).fetch_all(&pool).await.unwrap();
        assert_eq!(accounts, vec!["owner".to_owned()], "{table}");
    }
    // Ожидающие задания владельца остаются как есть, и их уникальность держится по паре.
    let payloads: Vec<Value> = sqlx::query_scalar(
        "select payload from scheduled_jobs where state='pending' order by kind",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        payloads,
        vec![json!({"provider":"claude"}), json!({"provider":"claude"})]
    );
    let duplicate = sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) values($1,'limit_check',$2,null,now(),'{\"provider\":\"claude\"}')")
        .bind(Uuid::now_v7())
        .bind(owner)
        .execute(&pool)
        .await;
    assert!(duplicate.is_err(), "one pending check per owner pair");
    sqlx::query("insert into scheduled_jobs(id,kind,owner_id,conversation_id,run_at,payload) values($1,'limit_check',$2,null,now(),'{\"provider\":\"claude\",\"account\":\"b-acct\"}')")
        .bind(Uuid::now_v7())
        .bind(owner)
        .execute(&pool)
        .await
        .unwrap();
    // Строка нового провайдера и учётной записи не мешает старой.
    sqlx::query("insert into provider_limits(provider,account) values('claude','b-acct')")
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query("insert into machine_harnesses(machine_id,harness,provider,title,account) values($1,'x','claude','t','Bad Label')")
            .bind(machine)
            .execute(&pool)
            .await
            .is_err(),
        "a label outside [a-z0-9-] is refused by the table"
    );
}
