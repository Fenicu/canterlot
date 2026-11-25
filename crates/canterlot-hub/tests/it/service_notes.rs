//! Служебные строки, которые не будят оркестратор: приём делегата и старт из
//! очереди запусков видны в ленте, но едут агенту приставкой к следующему вводу.

use std::time::Duration;

use canterlot_hub::{
    activity::Thresholds,
    config::{ActivityConfig, LimitsConfig},
    delivery::{
        self, ContentBlock, SubmitInput,
        dispatch::{DispatchOutcome, GateInputs, dispatch_next},
    },
    executions,
    model::ExecutionState,
    scheduler::{ClaimedJob, JobKind},
    session_tools::jobs,
    testkit::{self, Seed},
};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

async fn seed(pool: &PgPool) -> Seed {
    let seed = testkit::seed(pool).await;
    sqlx::query("update machines set availability='available' where id=$1")
        .bind(seed.machine_id)
        .execute(pool)
        .await
        .unwrap();
    seed
}

async fn start(pool: &PgPool, seed: &Seed) -> executions::StartedExecution {
    let execution = executions::start(pool, seed.owner_id, seed.conversation_id, Uuid::now_v7())
        .await
        .unwrap();
    for state in [ExecutionState::Starting, ExecutionState::Running] {
        executions::set_state(pool, execution.execution_id, state, None, Uuid::now_v7())
            .await
            .unwrap();
    }
    execution
}

fn accepted_origin(child: Uuid) -> Value {
    json!({
        "kind": "event",
        "from": child,
        "chain_depth": 0,
        "event": {"kind": "accepted", "session_id": child, "worktree": "removed"},
    })
}

fn failed_origin() -> Value {
    json!({
        "kind": "event",
        "chain_depth": 0,
        "event": {"kind": "spawn_queue", "state": "failed", "detail": "rate"},
    })
}

/// Задание заметки тем же путём, каким его ставит приём и очередь запусков.
async fn note(pool: &PgPool, seed: &Seed, origin: Value, text: &str) -> Uuid {
    let job = ClaimedJob {
        id: Uuid::now_v7(),
        kind: JobKind::SessionTask,
        owner_id: seed.owner_id,
        conversation_id: Some(seed.conversation_id),
        run_at: Utc::now(),
        payload: json!({"action": "note", "text": text, "origin": origin}),
        attempts: 0,
    };
    let thresholds = Thresholds::from(&ActivityConfig::default());
    let mut tx = pool.begin().await.unwrap();
    let outcome = jobs::run_in_tx(
        &mut tx,
        &job,
        Utc::now(),
        Uuid::now_v7(),
        None,
        &Default::default(),
        &thresholds,
        None,
        None,
        &LimitsConfig::default(),
        Duration::from_secs(60),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let canterlot_hub::crunch::JobOutcome::Done(value) = outcome else {
        panic!("note job did not finish: {outcome:?}");
    };
    value["input_id"].as_str().unwrap().parse().unwrap()
}

async fn owner_input(pool: &PgPool, seed: &Seed) -> delivery::SubmittedInput {
    delivery::submit_input(
        pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: "owner:one".to_owned(),
            content: vec![ContentBlock::Text {
                text: "собери релиз".to_owned(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap()
}

async fn dispatch(pool: &PgPool, seed: &Seed) -> DispatchOutcome {
    dispatch_next(
        pool,
        seed.conversation_id,
        GateInputs {
            machine_available: true,
            machine_reconciled: true,
            session_capabilities: Vec::new(),
        },
        Uuid::now_v7(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn an_accepted_event_is_a_quiet_feed_line_and_an_input_prefix() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = seed(&pool).await;
    start(&pool, &seed).await;
    let child = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
    let text = format!(
        "[canterlot] Session \"Делегат\" (id {child}, synthetic, model m) was accepted: it is stopped and archived."
    );
    let note = note(&pool, &seed, accepted_origin(child), &text).await;

    let (state, service, feed, attached): (String, bool, Option<i64>, Option<Uuid>) =
        sqlx::query_as(
            "select state,service_note,feed_seq,note_attached_to from inputs where id=$1",
        )
        .bind(note)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(state, "accepted");
    assert!(service);
    assert!(feed.is_some(), "строка видна в ленте");
    assert_eq!(attached, None);
    let queued: i64 = sqlx::query_scalar(
        "select count(*) from inputs where conversation_id=$1
           and state in ('queued','dispatched','on_machine')",
    )
    .bind(seed.conversation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(queued, 0, "служебная строка не начинает ход");
    let runs: i64 = sqlx::query_scalar("select count(*) from runs where conversation_id=$1")
        .bind(seed.conversation_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(runs, 0);
    let feed_line: bool = sqlx::query_scalar(
        "select exists(select 1 from events where kind='input_queued'
                        and payload->>'input_id'=$1::text)",
    )
    .bind(note)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(feed_line, "лента получает строку приёма");

    let owner = owner_input(&pool, &seed).await;
    let outcome = dispatch(&pool, &seed).await;
    let DispatchOutcome::Dispatched { command_id, .. } = outcome else {
        panic!("owner input did not dispatch: {outcome:?}");
    };
    let payload: Value = sqlx::query_scalar("select payload from commands where id=$1")
        .bind(command_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    let content = payload["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "{content:?}");
    assert_eq!(content[0]["text"], text);
    assert_eq!(content[1]["text"], "собери релиз");
    let owner_content: Value = sqlx::query_scalar("select content from inputs where id=$1")
        .bind(owner.input_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(owner_content[0]["text"], "собери релиз", "лента чистая");
    let attached: Option<Uuid> =
        sqlx::query_scalar("select note_attached_to from inputs where id=$1")
            .bind(note)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(attached, Some(owner.input_id));
}

#[tokio::test]
async fn a_failed_queued_spawn_still_wakes_the_orchestrator() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = seed(&pool).await;
    let note = note(
        &pool,
        &seed,
        failed_origin(),
        "[canterlot] Queued spawn q-00000000 failed: rate. Nothing was started.",
    )
    .await;
    let (state, service): (String, bool) =
        sqlx::query_as("select state,service_note from inputs where id=$1")
            .bind(note)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(state, "queued", "сбой очереди по-прежнему будит");
    assert!(!service);
}

async fn command_content(pool: &PgPool, command_id: Uuid) -> Value {
    sqlx::query_scalar("select payload->'content' from commands where id=$1")
        .bind(command_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Выдача не состоялась: машина сохранила команду и вернула ввод в очередь.
async fn dispatch_and_withdraw(pool: &PgPool, seed: &Seed) -> (Uuid, Uuid) {
    use canterlot_hub::delivery::receipts::{InputReceipt, apply_input_receipt};
    let outcome = dispatch(pool, seed).await;
    let DispatchOutcome::Dispatched {
        input_id,
        command_id,
        attempt,
        ..
    } = outcome
    else {
        panic!("queued input did not dispatch: {outcome:?}");
    };
    for receipt in [InputReceipt::Stored, InputReceipt::Withdrawn] {
        apply_input_receipt(pool, command_id, attempt, receipt, Uuid::now_v7())
            .await
            .unwrap();
    }
    (input_id, command_id)
}

const ACCEPTED: &str = "[canterlot] Session \"Делегат\" was accepted.";

/// Заметка приложена к вводу, выдача не состоялась, владелец поправил текст. Вторая
/// выдача несёт и заметку, и новый текст: правка не стирает приставку, а повтор без
/// правки её не удваивает.
#[tokio::test]
async fn a_note_survives_a_withdrawn_dispatch_and_an_edit_of_its_input() {
    use canterlot_hub::delivery::control::{EditInput, request_edit_input};
    let pool = canterlot_hub::testkit::db().await;
    let seed = seed(&pool).await;
    start(&pool, &seed).await;
    let note = note(&pool, &seed, accepted_origin(Uuid::now_v7()), ACCEPTED).await;
    let owner = owner_input(&pool, &seed).await;
    let (input, command) = dispatch_and_withdraw(&pool, &seed).await;
    assert_eq!(input, owner.input_id);
    assert_eq!(
        command_content(&pool, command).await,
        json!([{"type":"text","text":ACCEPTED},{"type":"text","text":"собери релиз"}])
    );
    // Повторная выдача того же ввода без правки.
    let (again, _) = dispatch_and_withdraw(&pool, &seed).await;
    assert_eq!(again, input);
    assert_eq!(
        command_content(&pool, command).await,
        json!([{"type":"text","text":ACCEPTED},{"type":"text","text":"собери релиз"}]),
        "the prefix is not doubled"
    );
    let revision: i64 = sqlx::query_scalar("select revision from inputs where id=$1")
        .bind(input)
        .fetch_one(&pool)
        .await
        .unwrap();
    request_edit_input(
        &pool,
        EditInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            input_id: input,
            text: "новый текст".into(),
            attachment_ids: Vec::new(),
            expected_revision: revision,
            idempotency_key: Uuid::now_v7().to_string(),
            correlation_id: Uuid::now_v7(),
        },
    )
    .await
    .unwrap();
    let outcome = dispatch(&pool, &seed).await;
    assert!(
        matches!(outcome, DispatchOutcome::Dispatched { input_id, .. } if input_id == input),
        "{outcome:?}"
    );
    assert_eq!(
        command_content(&pool, command).await,
        json!([{"type":"text","text":ACCEPTED},{"type":"text","text":"новый текст"}]),
        "the note rides with the edited input"
    );
    let attached: Option<Uuid> =
        sqlx::query_scalar("select note_attached_to from inputs where id=$1")
            .bind(note)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(attached, Some(input));
}

/// Ввод с приложенной заметкой вернулся в очередь, и владелец его отменил. Заметка не
/// остаётся привязанной к мёртвому вводу: её несёт следующий.
#[tokio::test]
async fn a_note_of_a_cancelled_input_rides_with_the_next_one() {
    let pool = canterlot_hub::testkit::db().await;
    let seed = seed(&pool).await;
    start(&pool, &seed).await;
    let note = note(&pool, &seed, accepted_origin(Uuid::now_v7()), ACCEPTED).await;
    owner_input(&pool, &seed).await;
    let (cancelled, _) = dispatch_and_withdraw(&pool, &seed).await;
    delivery::control::cancel_input(
        &pool,
        seed.owner_id,
        cancelled,
        &Uuid::now_v7().to_string(),
        Uuid::now_v7(),
    )
    .await
    .unwrap();
    let next = delivery::submit_input(
        &pool,
        SubmitInput {
            owner_id: seed.owner_id,
            conversation_id: seed.conversation_id,
            idempotency_key: "owner:two".to_owned(),
            content: vec![ContentBlock::Text {
                text: "другое".to_owned(),
            }],
            received_at: Utc::now(),
            correlation_id: Uuid::now_v7(),
            origin: None,
        },
    )
    .await
    .unwrap();
    let outcome = dispatch(&pool, &seed).await;
    let DispatchOutcome::Dispatched {
        input_id,
        command_id,
        ..
    } = outcome
    else {
        panic!("the next input did not dispatch: {outcome:?}");
    };
    assert_eq!(input_id, next.input_id);
    assert_eq!(
        command_content(&pool, command_id).await,
        json!([{"type":"text","text":ACCEPTED},{"type":"text","text":"другое"}])
    );
    let attached: Option<Uuid> =
        sqlx::query_scalar("select note_attached_to from inputs where id=$1")
            .bind(note)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(attached, Some(next.input_id));
}

mod send_now {
    //! «Отправить сейчас» собирает текст и привязывает заметки той же операцией, что
    //! обычная выдача.

    use canterlot_hub::delivery::{
        control::{SendNowOutcome, request_send_now},
        receipts::{InputReceipt, apply_input_receipt},
    };

    use super::*;

    async fn text_input(pool: &PgPool, seed: &Seed, text: &str) -> delivery::SubmittedInput {
        delivery::submit_input(
            pool,
            SubmitInput {
                owner_id: seed.owner_id,
                conversation_id: seed.conversation_id,
                idempotency_key: Uuid::now_v7().to_string(),
                content: vec![ContentBlock::Text { text: text.into() }],
                received_at: Utc::now(),
                correlation_id: Uuid::now_v7(),
                origin: None,
            },
        )
        .await
        .unwrap()
    }

    /// Сессия Codex с идущим ходом.
    async fn running_turn(pool: &PgPool) -> Seed {
        let seed = seed(pool).await;
        sqlx::query("update conversations set harness='codex' where id=$1")
            .bind(seed.conversation_id)
            .execute(pool)
            .await
            .unwrap();
        start(pool, &seed).await;
        text_input(pool, &seed, "первый ход").await;
        let DispatchOutcome::Dispatched {
            command_id,
            attempt,
            ..
        } = dispatch(pool, &seed).await
        else {
            panic!("the turn did not dispatch");
        };
        for receipt in [
            InputReceipt::Stored,
            InputReceipt::TransmissionStarted,
            InputReceipt::Accepted,
        ] {
            apply_input_receipt(pool, command_id, attempt, receipt, Uuid::now_v7())
                .await
                .unwrap();
        }
        seed
    }

    async fn steer(pool: &PgPool, seed: &Seed, input_id: Uuid) -> Uuid {
        let outcome = request_send_now(
            pool,
            seed.owner_id,
            seed.conversation_id,
            input_id,
            &Uuid::now_v7().to_string(),
            Uuid::now_v7(),
            &["send_now_v1".to_owned()],
        )
        .await
        .unwrap();
        let SendNowOutcome::Requested { steer_command_id } = outcome else {
            panic!("send now not requested: {outcome:?}");
        };
        steer_command_id
    }

    async fn bound(pool: &PgPool, note: Uuid) -> Option<Uuid> {
        sqlx::query_scalar("select note_attached_to from inputs where id=$1")
            .bind(note)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    const EARLIER: &str = "[canterlot] Session \"Ранний\" was accepted.";

    /// Вливание несёт свободную заметку и заметку, которую этому вводу уже привязала
    /// прежняя, не состоявшаяся выдача. Принятое вливание забирает обе: следующей
    /// выдаче они не достаются, но и в тексте вливания они были.
    #[tokio::test]
    async fn send_now_carries_the_service_notes_of_its_input() {
        let pool = canterlot_hub::testkit::db().await;
        let seed = running_turn(&pool).await;
        let earlier = note(&pool, &seed, accepted_origin(Uuid::now_v7()), EARLIER).await;
        let free = note(&pool, &seed, accepted_origin(Uuid::now_v7()), ACCEPTED).await;
        let steered = text_input(&pool, &seed, "влей это").await;
        // Состояние после выдачи, которая вернулась в очередь: заметка привязана к вводу.
        sqlx::query("update inputs set note_attached_to=$2 where id=$1")
            .bind(earlier)
            .bind(steered.input_id)
            .execute(&pool)
            .await
            .unwrap();
        let command = steer(&pool, &seed, steered.input_id).await;
        assert_eq!(
            command_content(&pool, command).await,
            json!([
                {"type":"text","text":format!("{EARLIER}\n\n{ACCEPTED}")},
                {"type":"text","text":"влей это"},
            ])
        );
        apply_input_receipt(&pool, command, 0, InputReceipt::Accepted, Uuid::now_v7())
            .await
            .unwrap();
        assert_eq!(bound(&pool, earlier).await, Some(steered.input_id));
        assert_eq!(bound(&pool, free).await, Some(steered.input_id));
    }

    /// Заметка, привязанная к вводу уже после того, как собрано вливание, в его тексте не
    /// была. Принятое вливание её отпускает: её несёт следующая выдача. Отказ вливания
    /// отпускает и те, что в нём были.
    #[tokio::test]
    async fn send_now_releases_notes_its_text_did_not_carry() {
        let pool = canterlot_hub::testkit::db().await;
        let seed = running_turn(&pool).await;
        let carried = note(&pool, &seed, accepted_origin(Uuid::now_v7()), EARLIER).await;
        let steered = text_input(&pool, &seed, "влей это").await;
        let command = steer(&pool, &seed, steered.input_id).await;
        assert_eq!(bound(&pool, carried).await, Some(steered.input_id));
        let late = note(&pool, &seed, accepted_origin(Uuid::now_v7()), ACCEPTED).await;
        sqlx::query("update inputs set note_attached_to=$2 where id=$1")
            .bind(late)
            .bind(steered.input_id)
            .execute(&pool)
            .await
            .unwrap();
        apply_input_receipt(&pool, command, 0, InputReceipt::Accepted, Uuid::now_v7())
            .await
            .unwrap();
        assert_eq!(bound(&pool, carried).await, Some(steered.input_id));
        assert_eq!(bound(&pool, late).await, None, "not in the steered text");

        let rejected = text_input(&pool, &seed, "и это").await;
        let command = steer(&pool, &seed, rejected.input_id).await;
        assert_eq!(bound(&pool, late).await, Some(rejected.input_id));
        apply_input_receipt(
            &pool,
            command,
            0,
            InputReceipt::ExplicitRejection {
                reason: "input_too_long".into(),
            },
            Uuid::now_v7(),
        )
        .await
        .unwrap();
        assert_eq!(bound(&pool, late).await, None, "a refused steer lets it go");
    }
}
