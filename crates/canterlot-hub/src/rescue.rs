//! Спасение хода, зависшего в генерации.
//!
//! Решение принимает проектор активности под блокировкой очереди: он уже видит
//! verdict `hung_in_generation` и все признаки сессии. Одно действие на эпизод
//! тишины и не больше одного автоматического действия на ход; эпизод кончается,
//! как только приходит новое потоковое событие. Инструментальные фазы сюда не
//! попадают — об этом заботится `activity::hung_in_generation`.

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgConnection;
use tracing::warn;
use uuid::Uuid;

use crate::{
    activity::{ActivityInputs, hung_in_generation},
    delivery::{ContentBlock, SubmitInput, control, submit_input_in_tx, submit_service_note_in_tx},
    notify,
    session::CAPABILITY_SEND_NOW_V1,
    session_tools::subs,
};

/// Что сделано с зависшим ходом. Записывается в служебную строку ленты и пуш.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RescueAction {
    /// Ввод влит в шаг OpenCode: он прерывает шаг и продолжает ход.
    Steered,
    /// Ход прерван, ожидающий ввод уйдёт следующим ходом.
    Interrupted,
    /// Ход прерван, толчок автопинка уйдёт следующим ходом.
    Nudged,
    /// Корневая сессия владельца без правила: ничего не прерываем, показываем карточку.
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

    /// Служебная строка ленты: только слова и цвет, без эмодзи и галочек.
    pub fn feed_text(self) -> Option<&'static str> {
        match self {
            Self::Steered => Some("Ход завис в генерации, влито и продолжено"),
            Self::Interrupted | Self::Nudged => Some("Ход завис в генерации, прерван и продолжен"),
            _ => None,
        }
    }
}

/// Текст толчка при спасении: короткий, английский, как остальные тексты толчков.
pub const HUNG_NUDGE_TEXT: &str =
    "Previous step hung in generation and was interrupted; continue from where you were";

/// Выбор действия — чистое решение по признакам сессии. Харнесс без надёжного
/// прерывания шага вливанием получает interrupt + доставку следующим ходом.
pub fn plan(
    harness: &str,
    capabilities: &[String],
    queued: bool,
    rule: bool,
    delegate: bool,
) -> RescueAction {
    if !delegate && !rule {
        return RescueAction::Notice;
    }
    if queued {
        return if harness == "opencode"
            && capabilities
                .iter()
                .any(|capability| capability == CAPABILITY_SEND_NOW_V1)
        {
            RescueAction::Steered
        } else {
            RescueAction::Interrupted
        };
    }
    if rule {
        RescueAction::Nudged
    } else {
        RescueAction::Interrupted
    }
}

#[derive(Clone, Copy)]
struct Episode {
    active: bool,
    rescued: bool,
    second_notified: bool,
}

/// Загрузить состояние эпизода, сбросив его при смене хода или исполнения.
async fn episode_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    execution_id: Uuid,
    turn: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Episode, sqlx::Error> {
    let row: Option<(Option<Uuid>, Option<DateTime<Utc>>, bool, bool, bool)> = sqlx::query_as(
        "select execution_id,turn_started_at,episode_active,rescued,second_notified
         from hung_rescues where conversation_id=$1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let current = match row {
        Some((Some(saved_execution), Some(saved_turn), active, rescued, second))
            if saved_execution == execution_id && saved_turn == turn =>
        {
            Episode {
                active,
                rescued,
                second_notified: second,
            }
        }
        _ => Episode {
            active: false,
            rescued: false,
            second_notified: false,
        },
    };
    sqlx::query(
        "insert into hung_rescues(conversation_id,execution_id,turn_started_at,
                                  episode_active,rescued,second_notified,updated_at)
         values($1,$2,$3,$4,$5,$6,$7)
         on conflict(conversation_id) do update set
           execution_id=excluded.execution_id,turn_started_at=excluded.turn_started_at,
           episode_active=excluded.episode_active,rescued=excluded.rescued,
           second_notified=excluded.second_notified,updated_at=excluded.updated_at",
    )
    .bind(conversation_id)
    .bind(execution_id)
    .bind(turn)
    .bind(current.active)
    .bind(current.rescued)
    .bind(current.second_notified)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(current)
}

async fn set_episode_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    active: bool,
    rescued: bool,
    second_notified: bool,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update hung_rescues set episode_active=$2,rescued=$3,second_notified=$4,updated_at=$5
         where conversation_id=$1",
    )
    .bind(conversation_id)
    .bind(active)
    .bind(rescued)
    .bind(second_notified)
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Служебная строка в ленту: есть в истории и приходит живым кадром, но очередь
/// её не видит и новый ход она не начинает.
#[allow(clippy::too_many_arguments)]
async fn note_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    owner_id: Uuid,
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
            idempotency_key: format!("rescue-note:{conversation_id}:{}", now.timestamp_millis()),
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

/// Один толчок автопинка вместо обычного ожидания: прерываем ход и ставим ввод
/// со счётчиком толчка, чтобы предел `max_consecutive` продолжал действовать.
#[allow(clippy::too_many_arguments)]
async fn nudge_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    owner_id: Uuid,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let rule: Option<(String, bool, i32, i32, i32)> = sqlx::query_as(
        "select state,enabled,consecutive,total,max_total from nudge_rules where conversation_id=$1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((state, enabled, consecutive, total, max_total)) = rule else {
        return Ok(false);
    };
    if !enabled || !matches!(state.as_str(), "watching" | "pending") || total >= max_total {
        return Ok(false);
    }
    let attempt = total + 1;
    let outcome = control::request_stop_in_tx(
        conn,
        owner_id,
        conversation_id,
        &format!("rescue-interrupt:{conversation_id}"),
        correlation_id,
    )
    .await;
    if let Err(error) = outcome {
        warn!(kind = "rescue_interrupt_failed", %conversation_id, error = %error, "hung nudge could not interrupt the turn");
        return Ok(false);
    }
    let origin = json!({
        "kind": "nudge",
        "rule_id": conversation_id,
        "attempt": attempt,
        "chain_depth": 0,
        "signals": {"hung_generation": true},
        "event": {"kind": "hung", "session_id": conversation_id}
    });
    match submit_input_in_tx(
        conn,
        &SubmitInput {
            owner_id,
            conversation_id,
            idempotency_key: format!("rescue-nudge:{conversation_id}:{attempt}"),
            content: vec![ContentBlock::Text {
                text: HUNG_NUDGE_TEXT.to_owned(),
            }],
            received_at: now,
            correlation_id,
            origin: Some(origin),
        },
    )
    .await
    {
        Ok(submitted) => {
            sqlx::query(
                "update nudge_rules set state='watching',held_fingerprint=null,consecutive=$2,
                     total=$3,updated_at=$4 where conversation_id=$1",
            )
            .bind(conversation_id)
            .bind(consecutive + 1)
            .bind(attempt)
            .bind(now)
            .execute(&mut *conn)
            .await?;
            sqlx::query(
                "insert into nudge_events(id,conversation_id,owner_id,at,attempt,signals,outcome,input_id)
                 values($1,$2,$3,$4,$5,$6,'nudged',$7)",
            )
            .bind(Uuid::now_v7())
            .bind(conversation_id)
            .bind(owner_id)
            .bind(now)
            .bind(attempt)
            .bind(json!({"hung_generation": true, "no_marker": true}))
            .bind(submitted.input_id)
            .execute(&mut *conn)
            .await?;
            Ok(true)
        }
        Err(error) => {
            warn!(kind = "rescue_nudge_failed", %conversation_id, error = %error, "hung nudge was not submitted");
            Ok(false)
        }
    }
}

/// Оценка за один пересчёт активности. Возвращает фактически выполненное действие.
#[allow(clippy::too_many_arguments)]
pub async fn evaluate_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    inputs: &ActivityInputs,
    thresholds: &crate::activity::Thresholds,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<RescueAction, sqlx::Error> {
    let Some(execution) = &inputs.execution else {
        return Ok(RescueAction::None);
    };
    let live = crate::executions::live(&mut *conn, conversation_id).await?;
    let Some((execution_id, _, _)) = live else {
        return Ok(RescueAction::None);
    };
    let turn = inputs.turn_started_at.or(execution.phase_at).unwrap_or(now);
    let turn_ms = turn.timestamp_millis();
    let hung = hung_in_generation(inputs, thresholds);
    if !hung {
        // Новое потоковое событие закрыло эпизод: следующее зависание в этом же
        // ходе считается вторым и получает только уведомление.
        sqlx::query(
            "update hung_rescues set episode_active=false,updated_at=$2
             where conversation_id=$1 and episode_active",
        )
        .bind(conversation_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;
        return Ok(RescueAction::None);
    }
    let episode = episode_in_tx(conn, conversation_id, execution_id, turn, now).await?;
    if episode.active {
        return Ok(RescueAction::None);
    }
    if episode.rescued {
        // Второе зависание того же хода: автоматических действий больше нет.
        set_episode_in_tx(conn, conversation_id, true, true, true, now).await?;
        if !episode.second_notified {
            notify::on_hung_in_tx(conn, conversation_id, execution_id, true).await?;
            let parent: Option<Uuid> =
                sqlx::query_scalar("select parent_conversation_id from conversations where id=$1")
                    .bind(conversation_id)
                    .fetch_optional(&mut *conn)
                    .await?
                    .flatten();
            if let Some(parent) = parent {
                let owner = inputs_owner(&mut *conn, conversation_id).await?;
                let text = format!(
                    "[canterlot] Session {conversation_id} hung in generation a second time \
                     after the rescue and was left interrupted. Continue it from where it stopped."
                );
                subs::insert_job(
                    conn,
                    owner,
                    parent,
                    now,
                    &json!({"action":"note","text":text,"origin":{"kind":"event","chain_depth":0}}),
                )
                .await?;
            }
        }
        return Ok(RescueAction::Second);
    }

    let (owner_id, harness, machine_id, parent): (Uuid, String, Uuid, Option<Uuid>) =
        sqlx::query_as(
            "select owner_id,harness,machine_id,parent_conversation_id from conversations where id=$1",
        )
        .bind(conversation_id)
        .fetch_one(&mut *conn)
        .await?;
    let capabilities: Vec<String> =
        sqlx::query_scalar("select coalesce(capabilities,'{}') from machines where id=$1")
            .bind(machine_id)
            .fetch_one(&mut *conn)
            .await?;
    let queued: Option<Uuid> = sqlx::query_scalar(
        "select id from inputs where conversation_id=$1 and state='queued'
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
    let action = plan(
        &harness,
        &capabilities,
        queued.is_some(),
        rule_active,
        parent.is_some(),
    );

    let done = match action {
        RescueAction::Steered => {
            let Some(input_id) = queued else {
                return Ok(RescueAction::None);
            };
            match control::request_send_now_in_tx(
                conn,
                owner_id,
                conversation_id,
                input_id,
                &format!("rescue-steer:{conversation_id}:{turn_ms}"),
                correlation_id,
                &capabilities,
            )
            .await
            {
                Ok(_) => true,
                Err(error) => {
                    warn!(kind = "rescue_steer_failed", %conversation_id, error = %error, "hung steer was not sent");
                    false
                }
            }
        }
        RescueAction::Interrupted => {
            match control::request_stop_in_tx(
                conn,
                owner_id,
                conversation_id,
                &format!("rescue-interrupt:{conversation_id}:{turn_ms}"),
                correlation_id,
            )
            .await
            {
                Ok(_) => {
                    if let Some(parent) = parent {
                        let text = format!(
                            "[canterlot] Session {conversation_id} hung in generation and was \
                             interrupted; continue it from where it stopped."
                        );
                        subs::insert_job(
                            conn,
                            owner_id,
                            parent,
                            now,
                            &json!({"action":"note","text":text,"origin":{"kind":"event","chain_depth":0}}),
                        )
                        .await?;
                    }
                    true
                }
                Err(error) => {
                    warn!(kind = "rescue_interrupt_failed", %conversation_id, error = %error, "hung turn was not interrupted");
                    false
                }
            }
        }
        RescueAction::Nudged => {
            nudge_in_tx(conn, conversation_id, owner_id, now, correlation_id).await?
        }
        RescueAction::Notice => {
            let origin = json!({
                "kind": "event",
                "chain_depth": 0,
                "event": {
                    "kind": "turn_hung",
                    "turn_hung_notice": true,
                    "session_id": conversation_id,
                    "queued_input_id": queued
                }
            });
            note_in_tx(
                conn,
                conversation_id,
                owner_id,
                "Ход завис".to_owned(),
                origin,
                now,
                correlation_id,
            )
            .await?;
            notify::on_hung_in_tx(conn, conversation_id, execution_id, false).await?;
            true
        }
        RescueAction::Second | RescueAction::None => false,
    };
    if !done {
        return Ok(RescueAction::None);
    }
    set_episode_in_tx(conn, conversation_id, true, true, false, now).await?;
    if let Some(text) = action.feed_text() {
        let origin = json!({
            "kind": "event",
            "chain_depth": 0,
            "event": {
                "kind": "turn_hung",
                "turn_hung_action": action.as_str(),
                "session_id": conversation_id
            }
        });
        note_in_tx(
            conn,
            conversation_id,
            owner_id,
            text.to_owned(),
            origin,
            now,
            correlation_id,
        )
        .await?;
    }
    Ok(action)
}

async fn inputs_owner(conn: &mut PgConnection, conversation_id: Uuid) -> Result<Uuid, sqlx::Error> {
    sqlx::query_scalar("select owner_id from conversations where id=$1")
        .bind(conversation_id)
        .fetch_one(conn)
        .await
}

/// Служебная строка для жесткого прерывания владельцем: та же лента, но без
/// автоматического продолжения. Пока используется карточкой PWA через HTTP.
pub async fn owner_interrupt_note_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    owner_id: Uuid,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    note_in_tx(
        conn,
        conversation_id,
        owner_id,
        "Ход завис в генерации, прерван".to_owned(),
        json!({"kind":"event","chain_depth":0,"event":{"kind":"turn_hung","turn_hung_action":"interrupted"}}),
        now,
        correlation_id,
    )
    .await
}

/// Служебная строка после «Влить» владельца.
pub async fn owner_steer_note_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    owner_id: Uuid,
    now: DateTime<Utc>,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    note_in_tx(
        conn,
        conversation_id,
        owner_id,
        "Ход завис в генерации, влито и продолжено".to_owned(),
        json!({"kind":"event","chain_depth":0,"event":{"kind":"turn_hung","turn_hung_action":"steered"}}),
        now,
        correlation_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_uses_steer_only_where_it_preempts_the_step() {
        let caps = vec![CAPABILITY_SEND_NOW_V1.to_owned()];
        assert_eq!(
            plan("opencode", &caps, true, false, true),
            RescueAction::Steered
        );
        assert_eq!(
            plan("opencode", &[], true, false, true),
            RescueAction::Interrupted
        );
        for harness in ["claude_code", "codex"] {
            assert_eq!(
                plan(harness, &caps, true, false, true),
                RescueAction::Interrupted
            );
        }
    }

    #[test]
    fn plan_rescues_delegates_and_nudged_sessions_but_not_plain_owner_roots() {
        let caps = vec![CAPABILITY_SEND_NOW_V1.to_owned()];
        assert_eq!(
            plan("opencode", &caps, false, false, false),
            RescueAction::Notice
        );
        assert_eq!(
            plan("opencode", &caps, false, true, false),
            RescueAction::Nudged
        );
        assert_eq!(
            plan("opencode", &caps, false, false, true),
            RescueAction::Interrupted
        );
    }

    #[test]
    fn feed_rows_are_words_only() {
        assert_eq!(
            RescueAction::Interrupted.feed_text(),
            Some("Ход завис в генерации, прерван и продолжен")
        );
        assert_eq!(
            RescueAction::Steered.feed_text(),
            Some("Ход завис в генерации, влито и продолжено")
        );
        assert_eq!(RescueAction::Notice.feed_text(), None);
        assert_eq!(RescueAction::Second.feed_text(), None);
    }
}
