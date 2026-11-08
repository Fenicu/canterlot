//! Задание `session_task`: в его транзакции живёт только очередь адресата, поэтому
//! две очереди вместе никогда не блокируются.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::{
    crunch::JobOutcome,
    delivery::{ContentBlock, SubmitError, SubmitInput, submit_input_in_tx},
    scheduler::ClaimedJob,
    session::Registry,
    session_tools::{accept, origin, spawn, summary},
};

#[derive(Deserialize)]
struct EventJob {
    target_id: Uuid,
    state: String,
    chain_depth: i32,
    #[serde(default)]
    armed_at: Option<DateTime<Utc>>,
    /// Граница отпущенного пачкой итога: у старых заданий её нет.
    #[serde(default)]
    boundary: Option<summary::Boundary>,
    /// Отпущенный после исчерпания автопинка итог.
    #[serde(default)]
    nudge_exhausted: bool,
    #[serde(default)]
    nudges: i64,
    #[serde(default)]
    asks_in_text: bool,
}

#[derive(Deserialize)]
struct NoteJob {
    text: String,
    #[serde(default)]
    origin: Option<Value>,
}

/// Событие простоя машины: текст собирается при доставке, потому что состояние цели
/// и время простоя к этому моменту могли измениться.
#[derive(Deserialize)]
struct MachineEventJob {
    target_id: Uuid,
    /// `unavailable`, `available`, `disk_low`, `disk_critical`, `mem_low`,
    /// `processes_leaked` или `lease_long_hold`.
    kind: String,
    /// Событие сторожа машины: подлежащее — машина, `target_id` — сам получатель.
    /// У заданий до сторожа поля нет, у них цель — сессия на машине.
    #[serde(default)]
    machine_id: Option<Uuid>,
    #[serde(default)]
    details: Value,
    #[serde(default)]
    outage_id: Option<Uuid>,
    #[serde(default)]
    free_gb: Option<u64>,
    #[serde(default)]
    free_pct: Option<u64>,
    #[serde(default)]
    chain_depth: i32,
    #[serde(default)]
    lease_kind: Option<String>,
    #[serde(default)]
    lease_label: Option<String>,
    #[serde(default)]
    owner_holder: bool,
    #[serde(default)]
    held_minutes: Option<i64>,
}

/// Действие задания `session_task`. Разбор исчерпывающий: новое действие обязано
/// получить обработчик в `run_in_tx` и `abandon_in_tx`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Event,
    MachineEvent,
    Note,
    Spawn,
    SpawnClose,
    Cleanup,
    AlarmDeliver,
    AlarmProbe,
    Nudge,
    SpawnDequeue,
    SpawnAttributes,
}

impl Action {
    #[cfg(test)]
    const ALL: [Self; 11] = [
        Self::Event,
        Self::MachineEvent,
        Self::Note,
        Self::Spawn,
        Self::SpawnClose,
        Self::Cleanup,
        Self::AlarmDeliver,
        Self::AlarmProbe,
        Self::Nudge,
        Self::SpawnDequeue,
        Self::SpawnAttributes,
    ];

    fn parse(value: &str) -> Option<Self> {
        match value {
            "event" => Some(Self::Event),
            "machine_event" => Some(Self::MachineEvent),
            "note" => Some(Self::Note),
            "spawn" => Some(Self::Spawn),
            "spawn_close" => Some(Self::SpawnClose),
            "cleanup" => Some(Self::Cleanup),
            "alarm_deliver" => Some(Self::AlarmDeliver),
            "alarm_probe" => Some(Self::AlarmProbe),
            "nudge" => Some(Self::Nudge),
            "spawn_dequeue" => Some(Self::SpawnDequeue),
            "spawn_attributes" => Some(Self::SpawnAttributes),
            _ => None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_in_tx(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    now: DateTime<Utc>,
    correlation_id: Uuid,
    registry: Option<&Registry>,
    settle: &crate::session_tools::settle::SettleConfig,
    thresholds: &crate::activity::Thresholds,
    spawn_runtime: Option<&crate::session_tools::spawn_queue::Runtime<'_>>,
    probe: Option<crate::session_tools::probe::PathProbe>,
) -> Result<JobOutcome, SubmitError> {
    let Some(subscriber) = job.conversation_id else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    let Some(name) = job.payload.get("action").and_then(Value::as_str) else {
        return Ok(JobOutcome::Cancelled("invalid_payload"));
    };
    let Some(action) = Action::parse(name) else {
        return Ok(JobOutcome::Cancelled("unsupported"));
    };
    match action {
        Action::Event => {
            let Ok(event) = serde_json::from_value::<EventJob>(job.payload.clone()) else {
                return Ok(JobOutcome::Cancelled("invalid_payload"));
            };
            let (text, origin) = event_message(conn, job.owner_id, &event).await?;
            deliver(
                conn,
                job,
                subscriber,
                text,
                Some(origin),
                now,
                correlation_id,
            )
            .await
        }
        Action::MachineEvent => {
            let Ok(event) = serde_json::from_value::<MachineEventJob>(job.payload.clone()) else {
                return Ok(JobOutcome::Cancelled("invalid_payload"));
            };
            let Some((text, origin)) =
                machine_event_message(conn, job.owner_id, &event, now).await?
            else {
                // Цель удалили: подписки больше нет, а отдельного события об удалении
                // подписчик уже не ждёт.
                return Ok(JobOutcome::Cancelled("target_deleted"));
            };
            deliver(
                conn,
                job,
                subscriber,
                text,
                Some(origin),
                now,
                correlation_id,
            )
            .await
        }
        Action::Note => {
            let Ok(note) = serde_json::from_value::<NoteJob>(job.payload.clone()) else {
                return Ok(JobOutcome::Cancelled("invalid_payload"));
            };
            deliver(
                conn,
                job,
                subscriber,
                note.text,
                note.origin,
                now,
                correlation_id,
            )
            .await
        }
        Action::Spawn => spawn::run_job(conn, job, subscriber, registry, now, correlation_id).await,
        Action::SpawnClose => spawn::close_job(conn, job, subscriber, correlation_id).await,
        Action::Cleanup => {
            Ok(accept::cleanup_job(conn, job, subscriber, now, correlation_id).await?)
        }
        Action::AlarmDeliver => {
            crate::session_tools::alarms::deliver_job_in_tx(conn, job, now, correlation_id).await
        }
        Action::AlarmProbe => Ok(crate::session_tools::alarms::probe_job_in_tx(
            conn,
            job,
            now,
            correlation_id,
            probe,
        )
        .await?),
        Action::Nudge => {
            crate::session_tools::nudge::run_job_in_tx(
                conn,
                job,
                now,
                correlation_id,
                settle,
                thresholds,
                probe,
            )
            .await
        }
        Action::SpawnAttributes => {
            let Some(target) = job
                .payload
                .get("conversation_id")
                .and_then(Value::as_str)
                .and_then(|value| Uuid::parse_str(value).ok())
            else {
                return Ok(JobOutcome::Cancelled("invalid_payload"));
            };
            let owned: bool = sqlx::query_scalar(
                "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
            )
            .bind(target)
            .bind(job.owner_id)
            .fetch_one(&mut *conn)
            .await?;
            if !owned {
                return Ok(JobOutcome::Cancelled("target_deleted"));
            }
            crate::conversations::emit_attributes(conn, job.owner_id, target, correlation_id)
                .await?;
            Ok(JobOutcome::Done(json!({"conversation_id": target})))
        }
        Action::SpawnDequeue => match spawn_runtime {
            Some(runtime) => {
                crate::session_tools::spawn_queue::run_job_in_tx(
                    conn,
                    job,
                    subscriber,
                    runtime,
                    now,
                    correlation_id,
                )
                .await
            }
            None => Ok(JobOutcome::Cancelled("runtime_missing")),
        },
    }
}

/// Цели уже нет, если её удалили: и своё событие об удалении, и ждавшее до него говорят одно.
async fn event_message(
    conn: &mut PgConnection,
    owner_id: Uuid,
    event: &EventJob,
) -> Result<(String, Value), sqlx::Error> {
    let id = event.target_id;
    let depth = event.chain_depth;
    // Удалённая и убранная в корзину цель не отдаёт ни названия, ни итога: только id.
    if event.state == "deleted" {
        return Ok((
            format!("[canterlot] Session {id} was deleted."),
            json!({"kind":"event","from":id,"chain_depth":depth,"event":origin::HubEvent::deleted(id)}),
        ));
    }
    let summary = summary::build_bounded(
        conn,
        owner_id,
        id,
        &event.state,
        event.armed_at,
        event.boundary.as_ref(),
    )
    .await?;
    Ok(match summary {
        Some(summary) => {
            let mut facts = summary.hub_event();
            if event.nudges > 0 {
                facts.nudges = Some(event.nudges);
            }
            if event.nudge_exhausted {
                facts.nudge_exhausted = Some(true);
            }
            if event.asks_in_text {
                facts.asks_in_text = Some(true);
            }
            let text = summary::render(&summary);
            let origin = json!({"kind":"event","from":id,"title":summary.title,"harness":summary.harness,"model":summary.model,"chain_depth":depth,"event":facts});
            (text, origin)
        }
        None => (
            format!("[canterlot] Session {id} was deleted."),
            json!({"kind":"event","from":id,"chain_depth":depth,"event":origin::HubEvent::deleted(id)}),
        ),
    })
}

/// Событие сторожа: текст о машине, отправитель в `origin` — сам получатель.
async fn machine_watch_message(
    conn: &mut PgConnection,
    owner_id: Uuid,
    machine_id: Uuid,
    event: &MachineEventJob,
    now: DateTime<Utc>,
) -> Result<Option<(String, Value)>, sqlx::Error> {
    let machine: Option<String> =
        sqlx::query_scalar("select name from machines where id=$1 and owner_id=$2")
            .bind(machine_id)
            .bind(owner_id)
            .fetch_optional(&mut *conn)
            .await?;
    let row: Option<(String, String, String, String, String)> = sqlx::query_as(
        "select c.title,c.harness,c.model,
                coalesce(d.activity_state,'unknown'),
                coalesce(d.activity_evidence->>'reason','')
         from conversations c
         left join conversation_derived d on d.conversation_id=c.id
         where c.id=$1 and c.owner_id=$2",
    )
    .bind(event.target_id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    let (Some(machine), Some((title, harness, model, state, reason))) = (machine, row) else {
        return Ok(None);
    };
    let machine = super::origin::clean_title(&machine);
    let (text, kind) = match event.kind.as_str() {
        "disk_low" => (
            super::machine_text::disk(&machine, false, &event.details, now),
            "machine_disk_low",
        ),
        "disk_critical" => (
            super::machine_text::disk(&machine, true, &event.details, now),
            "machine_disk_critical",
        ),
        "mem_low" => (
            super::machine_text::memory(&machine, &event.details),
            "machine_mem_low",
        ),
        "processes_leaked" => (
            super::machine_text::leaked(&machine, &event.details),
            "machine_processes_leaked",
        ),
        _ => return Ok(None),
    };
    let id = event.target_id;
    let origin = json!({
        "kind": "event",
        "from": id,
        "title": super::origin::clean_title(&title),
        "harness": harness,
        "model": model,
        "chain_depth": event.chain_depth,
        "event": {
            "kind": kind,
            "session_id": id,
            "state": machine_state(&state, &reason),
            "machine_id": machine_id,
        },
    });
    Ok(Some((text, origin)))
}

async fn machine_event_message(
    conn: &mut PgConnection,
    owner_id: Uuid,
    event: &MachineEventJob,
    now: DateTime<Utc>,
) -> Result<Option<(String, Value)>, sqlx::Error> {
    if let Some(machine_id) = event.machine_id {
        return machine_watch_message(conn, owner_id, machine_id, event, now).await;
    }
    let row: Option<(String, String, String, String, String, String)> = sqlx::query_as(
        "select c.title,c.harness,c.model,m.name,
                coalesce(d.activity_state,'unknown'),
                coalesce(d.activity_evidence->>'reason','')
         from conversations c join machines m on m.id=c.machine_id
         left join conversation_derived d on d.conversation_id=c.id
         where c.id=$1 and c.owner_id=$2",
    )
    .bind(event.target_id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((title, harness, model, machine, state, reason)) = row else {
        return Ok(None);
    };
    let title = super::origin::clean_title(&title);
    let machine = super::origin::clean_title(&machine);
    let id = event.target_id;
    let (text, state, kind) = if event.kind == "disk_low" || event.kind == "disk_critical" {
        let critical = event.kind == "disk_critical";
        let free_gb = event.free_gb.unwrap_or(0);
        let free_pct = event.free_pct.unwrap_or(0);
        (
            format!(
                "[canterlot] Machine \"{machine}\" is {}low on disk: \
                 {free_gb} GB free ({free_pct}%). Builds and new sessions may fail.",
                if critical { "critically " } else { "" }
            ),
            machine_state(&state, &reason),
            if critical {
                "machine_disk_critical"
            } else {
                "machine_disk_low"
            },
        )
    } else if event.kind == "lease_long_hold" {
        let lease = if event.lease_kind.as_deref() == Some("gate") {
            "gate"
        } else {
            "build"
        };
        let minutes = event.held_minutes.unwrap_or(0).max(1);
        let label = event
            .lease_label
            .as_deref()
            .map(super::origin::clean_title)
            .filter(|label| !label.is_empty())
            .map(|label| format!(" (\"{label}\")"))
            .unwrap_or_default();
        let holder = if event.owner_holder {
            "The owner's terminal".to_owned()
        } else {
            format!("Session \"{title}\" (id {id})")
        };
        (
            format!(
                "[canterlot] Machine \"{machine}\": {holder} has held the {lease} lease{label} \
                 for {minutes} min. Other heavy tasks on the machine queue behind it; \
                 check whether the command hangs."
            ),
            machine_state(&state, &reason),
            "lease_long_hold",
        )
    } else if event.kind == "available" {
        let state = machine_state(&state, &reason);
        (
            format!(
                "[canterlot] Machine \"{machine}\" is available again; \
                 session \"{title}\" (id {id}) is now {state}."
            ),
            state,
            "machine_available",
        )
    } else {
        let started: Option<DateTime<Utc>> =
            sqlx::query_scalar("select started_at from machine_outages where id=$1")
                .bind(event.outage_id)
                .fetch_optional(&mut *conn)
                .await?;
        let minutes = started
            .map(|at| (now - at).num_minutes().max(1))
            .unwrap_or(1);
        (
            format!(
                "[canterlot] Machine \"{machine}\" is unavailable for {minutes} min; \
                 session \"{title}\" (id {id}) state is unknown. \
                 It may resume when the machine reconnects."
            ),
            "unknown".to_owned(),
            "machine_unavailable",
        )
    };
    let origin = json!({
        "kind": "event",
        "from": id,
        "title": title,
        "harness": harness,
        "model": model,
        "chain_depth": event.chain_depth,
        "event": {"kind": kind, "session_id": id, "state": state},
    });
    Ok(Some((text, origin)))
}

/// Состояние цели словами для события о возврате машины. У сбоя называем причину:
/// потерянное исполнение видно подписчику как `failed (owner_lost)`.
fn machine_state(state: &str, reason: &str) -> String {
    match state {
        "working" => "running".to_owned(),
        "waiting_for_user" => "waiting for the owner".to_owned(),
        "quota_paused" => "paused by a limit".to_owned(),
        "failed" if !reason.is_empty() => format!("failed ({reason})"),
        "idle" | "stalled" | "failed" | "stopped" => state.to_owned(),
        _ => "unknown".to_owned(),
    }
}

async fn deliver(
    conn: &mut PgConnection,
    job: &ClaimedJob,
    subscriber: Uuid,
    text: String,
    origin: Option<Value>,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<JobOutcome, SubmitError> {
    let key = format!("sub:{}", job.id);
    let existing: Option<Uuid> =
        sqlx::query_scalar("select id from inputs where owner_id=$1 and idempotency_key=$2")
            .bind(job.owner_id)
            .bind(&key)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some(input_id) = existing {
        return Ok(JobOutcome::Done(json!({"input_id":input_id})));
    }
    let submitted = submit_input_in_tx(
        conn,
        &SubmitInput {
            owner_id: job.owner_id,
            conversation_id: subscriber,
            idempotency_key: key,
            content: vec![ContentBlock::Text { text }],
            received_at: now,
            correlation_id,
            origin,
        },
    )
    .await?;
    Ok(JobOutcome::Done(json!({"input_id":submitted.input_id})))
}

/// Задание исчерпало попытки или признано негодным: то, что оно должно было закрыть, закрывается
/// здесь, в той же транзакции. Остальным видам задания закрывать нечего.
/// `true` значит, что закрытие отложено чужой блокировкой и задание надо повторить позже.
pub async fn abandon_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Option<Uuid>,
    payload: &Value,
    now: DateTime<Utc>,
) -> Result<bool, sqlx::Error> {
    let Some(conversation) = conversation_id else {
        return Ok(false);
    };
    let correlation_id = Uuid::now_v7();
    let Some(name) = payload.get("action").and_then(Value::as_str) else {
        return Ok(false);
    };
    let Some(action) = Action::parse(name) else {
        return Ok(false);
    };
    match action {
        Action::Event | Action::Note | Action::MachineEvent => Ok(false),
        Action::Spawn => {
            spawn::abandon_spawn_in_tx(conn, conversation, payload, now, correlation_id)
                .await
                .map(|_| false)
        }
        Action::SpawnClose => {
            spawn::abandon_close_in_tx(conn, conversation, payload, correlation_id)
                .await
                .map(|_| false)
        }
        Action::Cleanup => {
            accept::abandon_cleanup_in_tx(conn, owner_id, conversation, now, correlation_id)
                .await
                .map(|_| false)
        }
        Action::AlarmDeliver => match crate::session_tools::alarms::abandon_deliver_in_tx(
            conn,
            conversation,
            payload,
            now,
        )
        .await?
        {
            crate::session_tools::alarms::Finalization::Finalized => Ok(false),
            crate::session_tools::alarms::Finalization::Deferred => Ok(true),
        },
        // Опрос лимитов ничего не удерживает: срок будильника закроет задание `alarm`.
        Action::AlarmProbe => Ok(false),
        Action::Nudge => {
            crate::session_tools::nudge::abandon_in_tx(conn, conversation, payload, now).await
        }
        Action::SpawnDequeue => {
            crate::session_tools::spawn_queue::abandon_in_tx(conn, conversation, payload, now)
                .await
                .map(|_| false)
        }
        // Обновление атрибутов нечего закрывать: следующее изменение опубликует их снова.
        Action::SpawnAttributes => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Разбор действия исчерпывающий в `abandon_in_tx`: новое действие без обработчика
    /// не собирается. Тест перечисляет те же варианты без `_`, чтобы это заметить.
    #[sqlx::test(migrations = "./migrations")]
    async fn every_action_kind_has_an_abandon_handler(
        pool: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let seed = crate::testkit::seed(&pool).await;
        let mut conn = pool.acquire().await?;
        for action in Action::ALL {
            let payload = match action {
                Action::Event => json!({"action": "event"}),
                Action::MachineEvent => json!({
                    "action": "machine_event",
                    "target_id": Uuid::now_v7(),
                    "kind": "unavailable",
                    "outage_id": Uuid::now_v7(),
                }),
                Action::Note => json!({"action": "note"}),
                Action::Spawn => json!({"action": "spawn", "request_id": Uuid::now_v7()}),
                Action::SpawnClose => json!({
                    "action": "spawn_close",
                    "request_id": Uuid::now_v7(),
                    "state": "decision_applied",
                    "reason": "job_failed",
                }),
                Action::Cleanup => json!({"action": "cleanup"}),
                Action::AlarmDeliver => {
                    json!({"action": "alarm_deliver", "alarm_id": Uuid::now_v7()})
                }
                Action::AlarmProbe => {
                    json!({"action": "alarm_probe", "alarm_id": Uuid::now_v7()})
                }
                Action::Nudge => json!({"action": "nudge", "fingerprint": "f"}),
                Action::SpawnDequeue => {
                    json!({"action": "spawn_dequeue", "queue_id": Uuid::now_v7()})
                }
                Action::SpawnAttributes => json!({
                    "action": "spawn_attributes",
                    "conversation_id": Uuid::now_v7(),
                }),
            };
            abandon_in_tx(
                &mut conn,
                seed.owner_id,
                Some(seed.conversation_id),
                &payload,
                Utc::now(),
            )
            .await?;
        }
        Ok(())
    }
}
