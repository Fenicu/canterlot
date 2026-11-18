//! Спасение хода, зависшего в генерации.
//!
//! Решение принимает проектор активности под блокировкой очереди по тому же условию
//! `activity::hung_turn`, по которому он выносит verdict: идущий ход слота `running`
//! с устойчивым именем, фаза `generating`, ни одного известного ожидания или
//! удержания, процесс жив и холост, окно наблюдения после старта собрано.
//!
//! Эпизод привязан к ходу: он открывается один раз за тишину и закрывается только
//! более новым прогрессом потока или концом хода, а не любым провалом условия.
//! Автоматическое действие — одно на ход; повторное зависание того же хода только
//! уведомляет. Действие считается выполненным, когда воркер подтвердил его
//! квитанцией; до неё запись хранит ожидающее действие.
//!
//! Что харнесс доказывает молчанием, описывает [`evidence_policy`]: где свидетельств
//! недостаточно, остаётся только карточка и пуш.

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use tracing::warn;
use uuid::Uuid;

use crate::{
    activity::{ActivityInputs, WINDOW_SAMPLES, hung_turn},
    config::HungRescue,
    delivery::{
        ContentBlock, SubmitInput,
        control::{self, InterruptGuard, StopOutcome},
        submit_service_note_in_tx,
    },
    notify,
    session::{CAPABILITY_GUARDED_INTERRUPT_V1, CAPABILITY_SEND_NOW_V1},
    session_tools::{
        nudge::{self, RescueNudge},
        subs,
    },
};

/// Что сделано с зависшим ходом. Записывается в служебную строку ленты и журнал.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RescueAction {
    /// Ввод влит в шаг OpenCode: он прерывает шаг и продолжает ход.
    Steered,
    /// Ход прерван, ожидающий ввод или заметка родителя уйдёт следующим ходом.
    Interrupted,
    /// Ход прерван, толчок автопинка уйдёт следующим ходом.
    Nudged,
    /// Автоматически нельзя или нечего делать: карточка владельцу и один пуш.
    Notice,
    /// Повторное зависание в том же ходе: только пуш и заметка родителю.
    Second,
    /// Действия не потребовалось (тот же эпизод, гонка или отказ доставки).
    None,
}

impl RescueAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Steered => "steered",
            Self::Interrupted => "interrupted",
            Self::Nudged => "nudged",
            Self::Notice => "notice",
            Self::Second => "second",
            Self::None => "none",
        }
    }

    /// Служебная строка ленты после подтверждения воркером: слова без эмодзи.
    pub fn feed_text(self) -> Option<&'static str> {
        match self {
            Self::Steered => Some("Ход завис в генерации, влито и продолжено"),
            Self::Interrupted | Self::Nudged => Some("Ход завис в генерации, прерван и продолжен"),
            _ => None,
        }
    }
}

/// Что харнесс доказывает молчанием хода.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvidencePolicy {
    /// Достаточно ли свидетельств для автоматического прерывания.
    pub auto: bool,
    /// На что опирается verdict.
    pub relies_on: &'static str,
    /// Чего доказать нельзя.
    pub gaps: &'static str,
}

/// Таблица свидетельств по харнессам. Автоматическое действие допускается только
/// там, где адаптер воркера сообщает всё, что считает признаком жизни хода.
pub fn evidence_policy(harness: &str) -> EvidencePolicy {
    match harness {
        "claude_code" => EvidencePolicy {
            auto: true,
            relies_on: "потоковые блоки и размышления (в том числе пустые дельты, \
                        thinking_tokens и api_retry как сигналы жизни), фаза, CPU и \
                        потомки CLI, фоновые задачи Claude",
            gaps: "",
        },
        "codex" => EvidencePolicy {
            auto: true,
            relies_on: "потоковые блоки, начало рассуждения до первой видимой дельты, \
                        активность субагентов, фаза, CPU и потомки сервера",
            gaps: "",
        },
        "opencode" => EvidencePolicy {
            auto: false,
            relies_on: "потоковые блоки и дельты рассуждения, фаза, CPU и потомки сервера",
            gaps: "повторы провайдера и фоновые задачи дочерних сессий во время хода \
                   родителя не видны хабу, процесс сервера общий для сессий",
        },
        _ => EvidencePolicy {
            auto: false,
            relies_on: "фаза, CPU и потомки процесса",
            gaps: "адаптер не сообщает признаков жизни хода",
        },
    }
}

/// Текст толчка при спасении: короткий, английский, как остальные тексты толчков.
pub const HUNG_NUDGE_TEXT: &str =
    "Previous step hung in generation and was interrupted; continue from where you were";

/// Выбор действия — чистое решение по признакам сессии. Автоматика требует режима
/// `auto`, достаточных свидетельств харнесса и охранного interrupt у воркера;
/// иначе владелец видит карточку.
#[allow(clippy::too_many_arguments)]
pub fn plan(
    mode: HungRescue,
    harness: &str,
    capabilities: &[String],
    queued: bool,
    rule: bool,
    delegate: bool,
) -> RescueAction {
    let has = |name: &str| capabilities.iter().any(|capability| capability == name);
    if mode != HungRescue::Auto || !evidence_policy(harness).auto {
        return RescueAction::Notice;
    }
    if !delegate && !rule {
        return RescueAction::Notice;
    }
    if queued && harness == "opencode" && has(CAPABILITY_SEND_NOW_V1) {
        return RescueAction::Steered;
    }
    if !has(CAPABILITY_GUARDED_INTERRUPT_V1) {
        return RescueAction::Notice;
    }
    if queued {
        return RescueAction::Interrupted;
    }
    if rule {
        RescueAction::Nudged
    } else {
        RescueAction::Interrupted
    }
}

/// Ожидающее действие: разговор, действие, владелец, эпизод и ход.
type PendingRow = (Uuid, Option<String>, Uuid, Option<Uuid>, Option<Uuid>);
/// Эпизод для карточки: его номер, активность, причина закрытия и ход.
type EpisodeRow = (Option<Uuid>, bool, Option<String>, Option<Uuid>);

#[derive(sqlx::FromRow, Clone)]
struct Row {
    execution_id: Option<Uuid>,
    turn_id: Option<Uuid>,
    progress_mark: Option<DateTime<Utc>>,
    episode_no: i32,
    episode_active: bool,
    rescued: bool,
    second_notified: bool,
    obs_execution_id: Option<Uuid>,
    obs_since: Option<DateTime<Utc>>,
    obs_last_at: Option<DateTime<Utc>>,
    obs_samples: i32,
    pending_command_id: Option<Uuid>,
}

async fn row_in_tx(conn: &mut PgConnection, conversation_id: Uuid) -> Result<Row, sqlx::Error> {
    sqlx::query("insert into hung_rescues(conversation_id) values($1) on conflict do nothing")
        .bind(conversation_id)
        .execute(&mut *conn)
        .await?;
    sqlx::query_as(
        "select execution_id,turn_id,progress_mark,episode_no,episode_active,
                rescued,second_notified,obs_execution_id,obs_since,obs_last_at,obs_samples,pending_command_id
         from hung_rescues where conversation_id=$1",
    )
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await
}

/// Окно наблюдения: пока после старта хаба, смены связи или старта исполнения не
/// набралось двух доказательных выборок подряд, verdict не выносится. Выборка
/// доказательна, если у самого CLI исполнения есть прошлый замер CPU и он жив.
pub async fn observe_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    inputs: &mut ActivityInputs,
    stale_after: std::time::Duration,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    inputs.hang.window_ready = false;
    let Some(execution) = inputs.hang.execution_id else {
        return Ok(());
    };
    let row = row_in_tx(conn, conversation_id).await?;
    let database: String = sqlx::query_scalar("select current_database()")
        .fetch_one(&mut *conn)
        .await?;
    let floor = [
        crate::activity::projector_started(&database),
        inputs.hang.link_since,
        inputs.hang.execution_created_at,
    ]
    .into_iter()
    .flatten()
    .max();
    let sample = inputs.hang.sample;
    let valid = inputs.link == crate::activity::LinkState::Available
        && inputs.process.is_some_and(|process| process.alive)
        && sample.is_some_and(|sample| {
            sample.cpu_valid == Some(true)
                && sample.cli_alive == Some(true)
                && floor.is_none_or(|floor| sample.observed_at >= floor)
        });
    let (samples, since, last) = match (valid, sample) {
        (true, Some(sample)) => {
            let same_run = row.obs_execution_id == Some(execution)
                && row.obs_since.is_some()
                && row.obs_since >= floor;
            match (same_run, row.obs_last_at) {
                (true, Some(last)) if sample.observed_at <= last => {
                    (row.obs_samples, row.obs_since, row.obs_last_at)
                }
                (true, Some(last))
                    if (sample.observed_at - last)
                        .to_std()
                        .is_ok_and(|gap| gap <= stale_after) =>
                {
                    (row.obs_samples + 1, row.obs_since, Some(sample.observed_at))
                }
                _ => (1, Some(sample.observed_at), Some(sample.observed_at)),
            }
        }
        _ => (0, None, None),
    };
    if samples != row.obs_samples
        || since != row.obs_since
        || last != row.obs_last_at
        || row.obs_execution_id != Some(execution)
    {
        sqlx::query(
            "update hung_rescues set obs_execution_id=$2,obs_since=$3,obs_last_at=$4,
                 obs_samples=$5,updated_at=$6 where conversation_id=$1",
        )
        .bind(conversation_id)
        .bind(execution)
        .bind(since)
        .bind(last)
        .bind(samples)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    }
    inputs.hang.window_ready = samples >= WINDOW_SAMPLES;
    Ok(())
}

/// Служебная строка в ленту: есть в истории и приходит живым кадром, но очередь
/// её не видит и новый ход она не начинает.
#[allow(clippy::too_many_arguments)]
async fn note_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    owner_id: Uuid,
    key: String,
    text: String,
    origin: serde_json::Value,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    match submit_service_note_in_tx(
        conn,
        &SubmitInput {
            owner_id,
            conversation_id,
            idempotency_key: key,
            content: vec![ContentBlock::Text { text }],
            received_at: now,
            correlation_id,
            origin: Some(origin),
        },
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(error) => {
            warn!(kind = "rescue_note_failed", %conversation_id, error = %error, "rescue note was not delivered");
            Ok(())
        }
    }
}

fn turn_event(kind: &str, conversation_id: Uuid, episode: Option<Uuid>) -> serde_json::Value {
    let mut event = json!({
        "kind": "turn_hung",
        "turn_hung_action": kind,
        "session_id": conversation_id,
    });
    if let Some(episode) = episode {
        event["turn_hung_episode"] = json!(episode);
    }
    json!({"kind": "event", "chain_depth": 0, "event": event})
}

enum Attempt {
    Done {
        action: RescueAction,
        pending: Option<(&'static str, Uuid)>,
    },
    /// Откат: ничего не записано, следующий пересчёт попробует снова.
    Retry,
    /// Автоматика недоступна здесь и сейчас: показать карточку.
    Notice,
}

/// Оценка за один пересчёт активности. Возвращает фактически выполненное действие.
pub async fn evaluate_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    inputs: &ActivityInputs,
    thresholds: &crate::activity::Thresholds,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<RescueAction, sqlx::Error> {
    let Some(execution_id) = inputs.hang.execution_id else {
        return Ok(RescueAction::None);
    };
    let row = row_in_tx(conn, conversation_id).await?;
    let hung = hung_turn(inputs, thresholds);
    // Конец хода закрывает эпизод: слот больше не `running` или ход другой.
    let current_turn = inputs.hang.turn_id.filter(|_| {
        matches!(inputs.queue.slot, crate::model::RunSlot::Running { .. }) && !inputs.wake_turn
    });
    let same_turn = row.execution_id == Some(execution_id)
        && current_turn.is_some()
        && row.turn_id == current_turn;
    if (current_turn.is_none() || !same_turn)
        && (row.episode_active || row.pending_command_id.is_some())
    {
        sqlx::query(
            "update hung_rescues set episode_active=false,pending_kind=null,
                 pending_command_id=null,pending_action=null,pending_since=null,
                 updated_at=$2 where conversation_id=$1",
        )
        .bind(conversation_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    }
    let Some(turn_id) = current_turn else {
        return Ok(RescueAction::None);
    };
    let mut row = row;
    if !same_turn {
        // Новый ход: счётчики хода обнуляются, номер эпизода идёт дальше.
        sqlx::query(
            "update hung_rescues set execution_id=$2,turn_id=$3,progress_mark=null,
                 episode_active=false,rescued=false,second_notified=false,resolved=null,
                 pending_kind=null,pending_command_id=null,pending_action=null,
                 pending_since=null,updated_at=$4 where conversation_id=$1",
        )
        .bind(conversation_id)
        .bind(execution_id)
        .bind(turn_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        row.execution_id = Some(execution_id);
        row.turn_id = Some(turn_id);
        row.progress_mark = None;
        row.episode_active = false;
        row.rescued = false;
        row.second_notified = false;
    }
    // Эпизод закрывает только более новый прогресс потока.
    if row.episode_active
        && let (Some(mark), Some(progress)) = (row.progress_mark, inputs.last_event_at)
        && progress > mark
    {
        sqlx::query(
            "update hung_rescues set episode_active=false,progress_mark=$2,
                 pending_kind=null,pending_command_id=null,pending_action=null,
                 pending_since=null,updated_at=$3 where conversation_id=$1",
        )
        .bind(conversation_id)
        .bind(progress)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        row.episode_active = false;
    }
    let Some(hung) = hung else {
        return Ok(RescueAction::None);
    };
    if row.episode_active {
        return Ok(RescueAction::None);
    }
    let episode_no = row.episode_no + 1;
    let episode_id = Uuid::now_v7();
    let (owner_id, harness, machine_id, parent): (Uuid, String, Uuid, Option<Uuid>) =
        sqlx::query_as(
            "select owner_id,harness,machine_id,parent_conversation_id from conversations where id=$1",
        )
        .bind(conversation_id)
        .fetch_one(&mut *conn)
        .await?;
    let open_episode = |active: bool, rescued: bool, second: bool| {
        let progress = inputs.last_event_at.or(Some(hung.silent_since));
        sqlx::query(
            "update hung_rescues set episode_no=$2,episode_id=$3,episode_active=$4,rescued=$5,
                 second_notified=$6,resolved=null,progress_mark=$7,updated_at=$8
             where conversation_id=$1",
        )
        .bind(conversation_id)
        .bind(episode_no)
        .bind(episode_id)
        .bind(active)
        .bind(rescued)
        .bind(second)
        .bind(progress)
        .bind(now)
    };

    if row.rescued {
        // Второе зависание того же хода: автоматических действий больше нет.
        open_episode(true, true, true).execute(&mut *conn).await?;
        if !row.second_notified {
            notify::on_hung_in_tx(conn, conversation_id, turn_id, true).await?;
            if parent.is_some() {
                let text = format!(
                    "[canterlot] Session {conversation_id} hung in generation again after the \
                     rescue; the turn is still hung and was not interrupted a second time. \
                     Check on it or interrupt it."
                );
                set_parent_note(conn, conversation_id, &text, now).await?;
            }
        }
        return Ok(RescueAction::Second);
    }

    let capabilities: Vec<String> =
        sqlx::query_scalar("select coalesce(capabilities,'{}') from machines where id=$1")
            .bind(machine_id)
            .fetch_one(&mut *conn)
            .await?;
    let queued: Option<Uuid> = sqlx::query_scalar(
        "select id from inputs where conversation_id=$1 and state='queued' and not service_note
         order by input_seq limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let rule_active: bool = sqlx::query_scalar(
        "select exists(select 1 from nudge_rules where conversation_id=$1
                       and enabled and state in ('watching','pending'))",
    )
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await?;
    let planned = plan(
        thresholds.hung_rescue,
        &harness,
        &capabilities,
        queued.is_some(),
        rule_active,
        parent.is_some(),
    );

    let mut attempt = Attempt::Notice;
    if matches!(
        planned,
        RescueAction::Steered | RescueAction::Interrupted | RescueAction::Nudged
    ) {
        sqlx::query("savepoint hung_rescue_action")
            .execute(&mut *conn)
            .await?;
        let result = act_in_tx(
            conn,
            planned,
            ActContext {
                conversation_id,
                owner_id,
                turn_id,
                episode_no,
                queued,
                checkpoint: inputs.hang.progress_checkpoint,
                capabilities: &capabilities,
                now,
                correlation_id,
            },
        )
        .await;
        match result {
            // Карточка вместо действия оставляет записанное правилом нуджа (исчерпание
            // уже выпустило удержанный итог), а откат нужен только повторяемой неудаче.
            Ok(done @ (Attempt::Done { .. } | Attempt::Notice)) => {
                sqlx::query("release savepoint hung_rescue_action")
                    .execute(&mut *conn)
                    .await?;
                attempt = done;
            }
            Ok(other) => {
                sqlx::query("rollback to savepoint hung_rescue_action")
                    .execute(&mut *conn)
                    .await?;
                attempt = other;
            }
            Err(error) => {
                // Прерывание, ввод толчка и счётчики фиксируются вместе или никак.
                sqlx::query("rollback to savepoint hung_rescue_action")
                    .execute(&mut *conn)
                    .await?;
                warn!(kind = "rescue_action_failed", %conversation_id, error = %error, "hung rescue was rolled back");
                attempt = Attempt::Retry;
            }
        }
    }

    match attempt {
        Attempt::Retry => Ok(RescueAction::None),
        Attempt::Done { action, pending } => {
            open_episode(true, true, false).execute(&mut *conn).await?;
            if let Some((kind, command_id)) = pending {
                sqlx::query(
                    "update hung_rescues set pending_kind=$2,pending_command_id=$3,
                         pending_action=$4,pending_since=$5 where conversation_id=$1",
                )
                .bind(conversation_id)
                .bind(kind)
                .bind(command_id)
                .bind(action.as_str())
                .bind(now)
                .execute(&mut *conn)
                .await?;
            }
            Ok(action)
        }
        Attempt::Notice => {
            open_episode(true, true, false).execute(&mut *conn).await?;
            let mut event = json!({
                "kind": "turn_hung",
                "turn_hung_notice": true,
                "turn_hung_episode": episode_id,
                "session_id": conversation_id
            });
            if let Some(input_id) = queued {
                event["queued_input_id"] = json!(input_id);
            }
            let origin = json!({"kind": "event", "chain_depth": 0, "event": event});
            note_in_tx(
                conn,
                conversation_id,
                owner_id,
                format!("rescue-notice:{conversation_id}:{turn_id}:{episode_no}"),
                "Ход завис".to_owned(),
                origin,
                now,
                correlation_id,
            )
            .await?;
            notify::on_hung_in_tx(conn, conversation_id, turn_id, false).await?;
            Ok(RescueAction::Notice)
        }
    }
}

struct ActContext<'a> {
    conversation_id: Uuid,
    owner_id: Uuid,
    turn_id: Uuid,
    episode_no: i32,
    queued: Option<Uuid>,
    checkpoint: u64,
    capabilities: &'a [String],
    now: DateTime<Utc>,
    correlation_id: Uuid,
}

/// Само действие внутри точки сохранения: любая ошибка откатывает его целиком.
async fn act_in_tx(
    conn: &mut PgConnection,
    planned: RescueAction,
    cx: ActContext<'_>,
) -> Result<Attempt, sqlx::Error> {
    let ActContext {
        conversation_id,
        owner_id,
        turn_id,
        episode_no,
        queued,
        checkpoint,
        capabilities,
        now,
        correlation_id,
    } = cx;
    match planned {
        RescueAction::Steered => {
            let Some(input_id) = queued else {
                return Ok(Attempt::Retry);
            };
            match control::request_send_now_in_tx(
                conn,
                owner_id,
                conversation_id,
                input_id,
                &format!("rescue-steer:{conversation_id}:{turn_id}:{episode_no}"),
                correlation_id,
                capabilities,
            )
            .await
            {
                Ok(outcome) => Ok(Attempt::Done {
                    action: RescueAction::Steered,
                    pending: outcome_command(&outcome).map(|id| ("steer", id)),
                }),
                Err(error) => {
                    warn!(kind = "rescue_steer_failed", %conversation_id, error = %error, "hung steer was not sent");
                    Ok(Attempt::Retry)
                }
            }
        }
        RescueAction::Interrupted | RescueAction::Nudged => {
            let nudge_sent = if planned == RescueAction::Nudged {
                match nudge::rescue_nudge_in_tx(
                    conn,
                    conversation_id,
                    turn_id,
                    episode_no,
                    HUNG_NUDGE_TEXT,
                    now,
                    correlation_id,
                )
                .await
                {
                    Ok(RescueNudge::Sent { .. }) => true,
                    // Правило исчерпано, не следит или нечем писать: прерывать нечем
                    // продолжить, показываем карточку вместо слепого прерывания.
                    Ok(RescueNudge::NotEligible | RescueNudge::Exhausted(_)) => {
                        return Ok(Attempt::Notice);
                    }
                    Ok(RescueNudge::Busy) => return Ok(Attempt::Retry),
                    Err(error) => {
                        warn!(kind = "rescue_nudge_failed", %conversation_id, error = %error, "hung nudge was not submitted");
                        return Ok(Attempt::Retry);
                    }
                }
            } else {
                false
            };
            let guard = InterruptGuard {
                turn_id,
                progress_checkpoint: checkpoint,
            };
            match control::request_stop_guarded_in_tx(
                conn,
                owner_id,
                conversation_id,
                &format!("rescue-interrupt:{conversation_id}:{turn_id}:{episode_no}"),
                correlation_id,
                Some(guard),
            )
            .await
            {
                Ok(outcome) => {
                    let Some(command_id) = outcome_command(&outcome) else {
                        // Живого исполнения нет: прерывать нечего, nudge откатывается.
                        return Ok(Attempt::Retry);
                    };
                    Ok(Attempt::Done {
                        action: if nudge_sent {
                            RescueAction::Nudged
                        } else {
                            RescueAction::Interrupted
                        },
                        pending: Some(("interrupt", command_id)),
                    })
                }
                Err(error) => {
                    warn!(kind = "rescue_interrupt_failed", %conversation_id, error = %error, "hung turn was not interrupted");
                    Ok(Attempt::Retry)
                }
            }
        }
        _ => Ok(Attempt::Retry),
    }
}

fn outcome_command(outcome: &impl OutcomeCommand) -> Option<Uuid> {
    outcome.command_id()
}

trait OutcomeCommand {
    fn command_id(&self) -> Option<Uuid>;
}

impl OutcomeCommand for StopOutcome {
    fn command_id(&self) -> Option<Uuid> {
        match self {
            StopOutcome::Requested {
                interrupt_command_id,
                ..
            } => *interrupt_command_id,
        }
    }
}

impl OutcomeCommand for control::SendNowOutcome {
    fn command_id(&self) -> Option<Uuid> {
        match self {
            control::SendNowOutcome::Requested { steer_command_id }
            | control::SendNowOutcome::AlreadyRequested { steer_command_id } => {
                Some(*steer_command_id)
            }
        }
    }
}

/// Отложить заметку родителю: она уходит после фиксации ребёнка (`flush_parent_note`).
async fn set_parent_note(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    text: &str,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("update hung_rescues set parent_note=$2,updated_at=$3 where conversation_id=$1")
        .bind(conversation_id)
        .bind(json!({"text": text}))
        .bind(now)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Доставить отложенную заметку родителю отдельной транзакцией после фиксации
/// ребёнка. Строку родителя берём без ожидания: пока её держит удаление, заметка
/// остаётся в записи и уходит со следующим обходом; удалённому родителю не уходит.
pub async fn flush_parent_note(pool: &PgPool, conversation_id: Uuid) -> Result<bool, sqlx::Error> {
    crate::db::in_tx::<_, sqlx::Error, _>(pool, move |conn| {
        Box::pin(async move {
            let pending: Option<(serde_json::Value, Option<Uuid>, Uuid)> = sqlx::query_as(
                "select h.parent_note,c.parent_conversation_id,c.owner_id
                 from hung_rescues h join conversations c on c.id=h.conversation_id
                 where h.conversation_id=$1 and h.parent_note is not null
                 for update of h skip locked",
            )
            .bind(conversation_id)
            .fetch_optional(&mut *conn)
            .await?;
            let Some((note, parent, owner_id)) = pending else {
                return Ok(false);
            };
            let Some(parent) = parent else {
                // Родителя нет (удалён и отвязан): получателя нет, заметку снимаем.
                sqlx::query("update hung_rescues set parent_note=null where conversation_id=$1")
                    .bind(conversation_id)
                    .execute(&mut *conn)
                    .await?;
                return Ok(false);
            };
            let locked: Option<Uuid> = sqlx::query_scalar(
                "select id from conversations where id=$1 and trashed_at is null
                 for key share skip locked",
            )
            .bind(parent)
            .fetch_optional(&mut *conn)
            .await?;
            if locked.is_none() {
                let exists: bool = sqlx::query_scalar(
                    "select exists(select 1 from conversations where id=$1 and trashed_at is null)",
                )
                .bind(parent)
                .fetch_one(&mut *conn)
                .await?;
                if exists {
                    // Строку держит удаление или другая транзакция: повторим позже.
                    return Ok(false);
                }
                sqlx::query("update hung_rescues set parent_note=null where conversation_id=$1")
                    .bind(conversation_id)
                    .execute(&mut *conn)
                    .await?;
                return Ok(false);
            }
            let text = note["text"].as_str().unwrap_or_default().to_owned();
            subs::insert_job(
                conn,
                owner_id,
                parent,
                Utc::now(),
                &json!({"action":"note","text":text,"origin":{"kind":"event","chain_depth":0}}),
            )
            .await?;
            sqlx::query("update hung_rescues set parent_note=null where conversation_id=$1")
                .bind(conversation_id)
                .execute(&mut *conn)
                .await?;
            Ok(true)
        })
    })
    .await
}

/// Очередь разговора берётся раньше записи спасения: тот же порядок, что у проектора.
async fn lock_queue_of_pending(
    conn: &mut PgConnection,
    command_id: Uuid,
) -> Result<(), sqlx::Error> {
    let conversation: Option<Uuid> =
        sqlx::query_scalar("select conversation_id from hung_rescues where pending_command_id=$1")
            .bind(command_id)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some(conversation) = conversation {
        sqlx::query("select 1 from queue_state where conversation_id=$1 for update")
            .bind(conversation)
            .fetch_optional(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Квитанция воркера на interrupt спасения. Подтверждённое прерывание пишет в ленту
/// «прерван и продолжен» и отдаёт родителю заметку (только если продолжать нечем);
/// отказ воркера (ход или прогресс изменились, прерывать нечего) показывается явно
/// и закрывает эпизод: следующее зависание этого хода получит уведомление.
pub async fn on_interrupt_status_in_tx(
    conn: &mut PgConnection,
    command_id: Uuid,
    status: &str,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    lock_queue_of_pending(conn, command_id).await?;
    let Some((conversation_id, action, owner_id, episode, turn)): Option<PendingRow> =
        sqlx::query_as(
            "select h.conversation_id,h.pending_action,c.owner_id,h.episode_id,h.turn_id
         from hung_rescues h join conversations c on c.id=h.conversation_id
         where h.pending_command_id=$1 and h.pending_kind='interrupt'
         for update of h",
        )
        .bind(command_id)
        .fetch_optional(&mut *conn)
        .await?
    else {
        return Ok(());
    };
    match status {
        "sent_to_agent" => return Ok(()),
        "acknowledged" | "completed" => {
            sqlx::query(
                "update hung_rescues set pending_kind=null,pending_command_id=null,
                     pending_action=null,pending_since=null,last_outcome=$2,updated_at=$3
                 where conversation_id=$1",
            )
            .bind(conversation_id)
            .bind(action.as_deref())
            .bind(now)
            .execute(&mut *conn)
            .await?;
            let parent: Option<Uuid> =
                sqlx::query_scalar("select parent_conversation_id from conversations where id=$1")
                    .bind(conversation_id)
                    .fetch_one(&mut *conn)
                    .await?;
            let continues = action.as_deref() == Some("nudged") || {
                let queued: bool = sqlx::query_scalar(
                    "select exists(select 1 from inputs where conversation_id=$1
                                   and state='queued' and not service_note)",
                )
                .bind(conversation_id)
                .fetch_one(&mut *conn)
                .await?;
                queued
            };
            if parent.is_some() && !continues {
                let text = format!(
                    "[canterlot] Session {conversation_id} hung in generation and was \
                     interrupted; continue it from where it stopped."
                );
                set_parent_note(conn, conversation_id, &text, now).await?;
            }
            let kind = action.as_deref().unwrap_or("interrupted");
            note_in_tx(
                conn,
                conversation_id,
                owner_id,
                format!(
                    "rescue-done:{conversation_id}:{}:{command_id}",
                    turn.map(|t| t.to_string()).unwrap_or_default()
                ),
                "Ход завис в генерации, прерван и продолжен".to_owned(),
                turn_event(kind, conversation_id, episode),
                now,
                correlation_id,
            )
            .await?;
        }
        _ => {
            sqlx::query(
                "update hung_rescues set pending_kind=null,pending_command_id=null,
                     pending_action=null,pending_since=null,last_outcome='refused',
                     episode_active=false,updated_at=$2 where conversation_id=$1",
            )
            .bind(conversation_id)
            .bind(now)
            .execute(&mut *conn)
            .await?;
            note_in_tx(
                conn,
                conversation_id,
                owner_id,
                format!("rescue-refused:{conversation_id}:{command_id}"),
                "Ход завис в генерации, прерывание не принято: ход уже изменился".to_owned(),
                turn_event("refused", conversation_id, episode),
                now,
                correlation_id,
            )
            .await?;
        }
    }
    Ok(())
}

/// Квитанция на вливание: отложенное или отклонённое вливание не считается спасением,
/// ввод остаётся в очереди за зависшим ходом, и это видно в ленте.
pub async fn on_steer_receipt_in_tx(
    conn: &mut PgConnection,
    command_id: Uuid,
    delivered: bool,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    lock_queue_of_pending(conn, command_id).await?;
    let Some((conversation_id, owner_id, episode)): Option<(Uuid, Uuid, Option<Uuid>)> =
        sqlx::query_as(
            "select h.conversation_id,c.owner_id,h.episode_id
             from hung_rescues h join conversations c on c.id=h.conversation_id
             where h.pending_command_id=$1 and h.pending_kind='steer' for update of h",
        )
        .bind(command_id)
        .fetch_optional(&mut *conn)
        .await?
    else {
        return Ok(());
    };
    sqlx::query(
        "update hung_rescues set pending_kind=null,pending_command_id=null,pending_action=null,
             pending_since=null,last_outcome=$2,episode_active=case when $3 then episode_active else false end,
             updated_at=$4 where conversation_id=$1",
    )
    .bind(conversation_id)
    .bind(if delivered { "steered" } else { "refused" })
    .bind(delivered)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    let (text, kind) = if delivered {
        ("Ход завис в генерации, влито и продолжено", "steered")
    } else {
        (
            "Ход завис в генерации, влить не удалось: сообщение уйдёт следующим ходом",
            "refused",
        )
    };
    note_in_tx(
        conn,
        conversation_id,
        owner_id,
        format!("rescue-steer-done:{conversation_id}:{command_id}"),
        text.to_owned(),
        turn_event(kind, conversation_id, episode),
        now,
        correlation_id,
    )
    .await
}

/// Состояние эпизода для карточки PWA и API её кнопок.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct EpisodeView {
    pub episode_id: Uuid,
    /// `open`: карточка действует; иначе причина, по которой она закрыта.
    pub state: String,
}

pub async fn episode_state_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    episode_id: Uuid,
) -> Result<EpisodeView, sqlx::Error> {
    let live: Option<EpisodeRow> = sqlx::query_as(
        "select h.episode_id,h.episode_active,h.resolved,h.turn_id from hung_rescues h
         where h.conversation_id=$1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let current_turn: Option<Uuid> = sqlx::query_scalar(
        "select i.command_id from queue_state q join inputs i on i.id=q.run_slot_input_id
         where q.conversation_id=$1 and q.run_slot='running'",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let state = match live {
        Some((Some(current), active, resolved, turn)) if current == episode_id => {
            if let Some(reason) = resolved {
                reason
            } else if !active {
                "progress".to_owned()
            } else if turn.is_none() || turn != current_turn {
                "turn_ended".to_owned()
            } else {
                "open".to_owned()
            }
        }
        _ => "superseded".to_owned(),
    };
    Ok(EpisodeView { episode_id, state })
}

/// Действия владельца по карточке «Ход завис» привязаны к эпизоду: закрытая карточка
/// (ход продолжился, закончился, эпизод сменился или действие уже выполнено) отвечает
/// отказом `hung_episode_resolved` и не трогает чужой ход.
pub async fn owner_interrupt(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    episode_id: Uuid,
    idempotency_key: &str,
    correlation_id: Uuid,
    guarded: bool,
) -> Result<StopOutcome, control::ControlError> {
    let key = idempotency_key.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            owner_gate_in_tx(conn, owner_id, conversation_id, episode_id).await?;
            let (turn, execution): (Uuid, Option<Uuid>) = sqlx::query_as(
                "select turn_id,execution_id from hung_rescues where conversation_id=$1",
            )
            .bind(conversation_id)
            .fetch_one(&mut *conn)
            .await?;
            let checkpoint: Option<i64> = sqlx::query_scalar(
                "select max(committed_seq) from journal_cursors where execution_id=$1",
            )
            .bind(execution)
            .fetch_one(&mut *conn)
            .await?;
            let guard = guarded.then_some(InterruptGuard {
                turn_id: turn,
                progress_checkpoint: checkpoint.unwrap_or(0).max(0) as u64,
            });
            let outcome = control::request_stop_guarded_in_tx(
                conn,
                owner_id,
                conversation_id,
                &key,
                correlation_id,
                guard,
            )
            .await?;
            let command = outcome_command(&outcome);
            sqlx::query(
                "update hung_rescues set resolved='owner_interrupt',
                     pending_kind=case when $2::uuid is null then null else 'interrupt' end,
                     pending_command_id=$2,pending_action='interrupted',pending_since=clock_timestamp()
                 where conversation_id=$1",
            )
            .bind(conversation_id)
            .bind(command)
            .execute(&mut *conn)
            .await?;
            Ok(outcome)
        })
    })
    .await
}

pub async fn owner_steer(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    episode_id: Uuid,
    idempotency_key: &str,
    correlation_id: Uuid,
    capabilities: Vec<String>,
) -> Result<control::SendNowOutcome, control::ControlError> {
    let key = idempotency_key.to_owned();
    crate::db::in_tx(pool, move |conn| {
        Box::pin(async move {
            owner_gate_in_tx(conn, owner_id, conversation_id, episode_id).await?;
            let input: Option<Uuid> = sqlx::query_scalar(
                "select id from inputs where conversation_id=$1 and state='queued'
                 and not service_note order by input_seq limit 1",
            )
            .bind(conversation_id)
            .fetch_optional(&mut *conn)
            .await?;
            let Some(input) = input else {
                return Err(control::ControlError::InvalidRequest {
                    reason: "input_not_queued",
                });
            };
            let outcome = control::request_send_now_in_tx(
                conn,
                owner_id,
                conversation_id,
                input,
                &key,
                correlation_id,
                &capabilities,
            )
            .await?;
            sqlx::query(
                "update hung_rescues set resolved='owner_steer',pending_kind='steer',
                     pending_command_id=$2,pending_action='steered',pending_since=clock_timestamp()
                 where conversation_id=$1",
            )
            .bind(conversation_id)
            .bind(outcome_command(&outcome))
            .execute(&mut *conn)
            .await?;
            Ok(outcome)
        })
    })
    .await
}

/// Очередь разговора — первой, как у проектора; потом эпизод должен быть открыт.
async fn owner_gate_in_tx(
    conn: &mut PgConnection,
    owner_id: Uuid,
    conversation_id: Uuid,
    episode_id: Uuid,
) -> Result<(), control::ControlError> {
    let found: Option<Uuid> = sqlx::query_scalar(
        "select q.conversation_id from queue_state q join conversations c on c.id=q.conversation_id
         where q.conversation_id=$1 and c.owner_id=$2 for update of q",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_optional(&mut *conn)
    .await?;
    if found.is_none() {
        return Err(control::ControlError::NotFound);
    }
    let view = episode_state_in_tx(conn, conversation_id, episode_id).await?;
    if view.state != "open" {
        return Err(control::ControlError::InvalidRequest {
            reason: "hung_episode_resolved",
        });
    }
    Ok(())
}

pub async fn episode_state(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    episode_id: Uuid,
) -> Result<Option<EpisodeView>, sqlx::Error> {
    let owned: bool = sqlx::query_scalar(
        "select exists(select 1 from conversations where id=$1 and owner_id=$2)",
    )
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_one(pool)
    .await?;
    if !owned {
        return Ok(None);
    }
    let mut conn = pool.acquire().await?;
    Ok(Some(
        episode_state_in_tx(&mut conn, conversation_id, episode_id).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn automatic_actions_need_the_mode_reliable_evidence_and_the_guarded_interrupt() {
        let all = caps(&["send_now_v1", "guarded_interrupt_v1"]);
        for harness in ["claude_code", "codex"] {
            assert_eq!(
                plan(HungRescue::Auto, harness, &all, true, false, true),
                RescueAction::Interrupted
            );
            assert_eq!(
                plan(HungRescue::Auto, harness, &all, false, true, false),
                RescueAction::Nudged
            );
            assert_eq!(
                plan(HungRescue::Auto, harness, &all, false, false, true),
                RescueAction::Interrupted
            );
            // Без охранного interrupt воркера прерывать вслепую нельзя.
            assert_eq!(
                plan(HungRescue::Auto, harness, &[], true, false, true),
                RescueAction::Notice
            );
            assert_eq!(
                plan(HungRescue::Alert, harness, &all, true, false, true),
                RescueAction::Notice
            );
        }
        assert_eq!(
            plan(HungRescue::Auto, "opencode", &all, true, false, true),
            RescueAction::Notice,
            "opencode has no reliable evidence yet"
        );
    }

    #[test]
    fn a_plain_owner_root_session_only_gets_a_card() {
        let all = caps(&["send_now_v1", "guarded_interrupt_v1"]);
        assert_eq!(
            plan(HungRescue::Auto, "claude_code", &all, false, false, false),
            RescueAction::Notice
        );
    }

    #[test]
    fn the_evidence_table_names_what_each_verdict_relies_on() {
        for harness in ["claude_code", "codex", "opencode", "unknown"] {
            let policy = evidence_policy(harness);
            assert!(!policy.relies_on.is_empty(), "{harness}");
            assert_eq!(policy.auto, policy.gaps.is_empty(), "{harness}");
        }
    }

    #[test]
    fn feed_rows_are_words_only() {
        assert_eq!(
            RescueAction::Interrupted.feed_text(),
            Some("Ход завис в генерации, прерван и продолжен")
        );
        assert_eq!(RescueAction::Notice.feed_text(), None);
        assert_eq!(RescueAction::Second.feed_text(), None);
    }
}
