//! Рутины: отчёт по расписанию из контекста существующей сессии.
//!
//! Запуск `session_report` создаёт скрытую боковую копию родителя (`side_kind='report'`),
//! отдаёт ей промпт расписания и кладёт в ленту родителя карточку отчёта. Карточка не
//! ввод и не блок ленты: у неё нет места в журнале, очереди и счётчиках, поэтому агент
//! её никогда не получает, а родитель не оживает и не просыпается. Когда ход копии
//! заканчивается, её последний ответ становится текстом карточки, копия удаляется.

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{
    events::{self, NewEvent, Scope},
    schedules::{self, RunState},
};

/// Ход копии дольше этого срока обрывается: отчёт не собран.
pub const TURN_LIMIT: TimeDelta = TimeDelta::minutes(60);
/// Копия, не начавшая ход (родитель на OpenCode занят), ждёт не дольше.
pub const START_LIMIT: TimeDelta = TimeDelta::hours(2);
/// Сколько живёт текст отчёта в истории запусков.
pub const TEXT_RETENTION_DAYS: i64 = 30;
/// Предел текста отчёта: ответ длиннее обрезается по границе символа.
const TEXT_LIMIT: usize = 200_000;
/// Сколько карточек и копий разбирает один проход.
const BATCH: i64 = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportState {
    /// Копия собирает отчёт.
    Pending,
    /// Отчёт готов: в `body` ответ копии.
    Ready,
    /// Отчёт не собран: в `error` причина. «Повторить» — «Запустить сейчас» расписания.
    Failed,
}
impl ReportState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
    fn parse(value: &str) -> Result<Self, sqlx::Error> {
        match value {
            "pending" => Ok(Self::Pending),
            "ready" => Ok(Self::Ready),
            "failed" => Ok(Self::Failed),
            _ => Err(sqlx::Error::Decode("invalid routine report state".into())),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReportError {
    /// Код причины: `no_native_session`, `copy_failed`, `turn_timeout` и т. п.
    pub reason: String,
    /// Готовая строка для карточки.
    pub message: String,
}

/// Карточка отчёта в ленте родителя.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct RoutineReportView {
    pub id: Uuid,
    /// Разговор, в ленте которого лежит карточка.
    pub conversation_id: Uuid,
    pub schedule_id: Uuid,
    /// Запуск расписания; пустой, если журнал запусков его уже убрал.
    pub run_id: Option<Uuid>,
    /// Заголовок из шаблона расписания; клиент показывает его как «Рутина · …».
    pub title: String,
    pub state: ReportState,
    /// Текст отчёта в Markdown: ответ копии на промпт расписания.
    pub body: Option<String>,
    pub error: Option<ReportError>,
    #[serde(with = "crate::activity::time")]
    pub created_at: DateTime<Utc>,
    #[serde(default, with = "schedules::time_opt")]
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(sqlx::FromRow)]
struct CardRow {
    id: Uuid,
    owner_id: Uuid,
    conversation_id: Uuid,
    schedule_id: Uuid,
    run_id: Option<Uuid>,
    copy_id: Option<Uuid>,
    title: String,
    state: String,
    body: Option<String>,
    error: Option<String>,
    turn_started_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    completed_at: Option<DateTime<Utc>>,
    dismissed_at: Option<DateTime<Utc>>,
}
impl CardRow {
    fn view(&self) -> Result<RoutineReportView, sqlx::Error> {
        Ok(RoutineReportView {
            id: self.id,
            conversation_id: self.conversation_id,
            schedule_id: self.schedule_id,
            run_id: self.run_id,
            title: self.title.clone(),
            state: ReportState::parse(&self.state)?,
            body: self.body.clone(),
            error: self.error.as_deref().map(|reason| ReportError {
                reason: reason.to_owned(),
                message: error_message(reason),
            }),
            created_at: self.created_at,
            completed_at: self.completed_at,
        })
    }
}

/// Строка для карточки по коду причины.
pub fn error_message(reason: &str) -> String {
    match reason {
        "no_native_session" => "У сессии ещё нет записи, которую можно прочитать",
        "conversation_not_found" => "Сессия удалена или недоступна",
        "report_capability_missing" => "Воркер машины не умеет боковой чат",
        "copy_failed" => "Копия сессии, собиравшая отчёт, завершилась с ошибкой",
        "copy_lost" => "Копия сессии пропала до конца отчёта",
        "turn_timeout" => "Копия не закончила отчёт за 60 минут",
        "parent_busy_timeout" => "Сессия была занята больше двух часов, отчёт не начался",
        "copy_not_started" => "Копия не начала работу за два часа",
        "empty_report" => "Копия закончила работу без текста ответа",
        "input_rejected" => "Копия отклонила запрос отчёта",
        "create_failed" | "submit_failed" => "Не удалось запустить копию сессии",
        "database_unavailable" => "База была недоступна",
        other => return format!("Не удалось собрать отчёт ({other})"),
    }
    .to_owned()
}

fn clip(text: &str) -> String {
    if text.len() <= TEXT_LIMIT {
        return text.to_owned();
    }
    let mut cut = TEXT_LIMIT;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    text[..cut].to_owned()
}

async fn emit_changed(
    conn: &mut PgConnection,
    view: &RoutineReportView,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: view.conversation_id,
            },
            kind: "routine_report_changed",
            conversation_id: Some(view.conversation_id),
            execution_id: None,
            payload: serde_json::to_value(view).map_err(|e| sqlx::Error::Encode(Box::new(e)))?,
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

async fn emit_dismissed(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    report_id: Uuid,
    schedule_id: Uuid,
    reason: &str,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    events::append(
        conn,
        NewEvent {
            scope: Scope::Conversation {
                id: conversation_id,
            },
            kind: "routine_report_dismissed",
            conversation_id: Some(conversation_id),
            execution_id: None,
            payload: json!({
                "report_id": report_id,
                "conversation_id": conversation_id,
                "schedule_id": schedule_id,
                "reason": reason,
            }),
            correlation_id,
        },
    )
    .await?;
    Ok(())
}

pub struct NewCard<'a> {
    pub owner_id: Uuid,
    pub conversation_id: Uuid,
    pub schedule_id: Uuid,
    pub run_id: Uuid,
    pub copy_id: Option<Uuid>,
    pub title: &'a str,
    /// `Pending` для собирающейся копии, `Failed` с причиной — когда запуск не дошёл до копии.
    pub state: ReportState,
    pub error: Option<&'a str>,
}

/// Новая открытая карточка расписания. Прежняя недосмотренная карточка того же расписания
/// в этом разговоре закрывается причиной `replaced`: открытая у расписания одна.
pub async fn open_in_tx(
    conn: &mut PgConnection,
    card: NewCard<'_>,
    correlation_id: Uuid,
) -> Result<Uuid, sqlx::Error> {
    let replaced: Vec<Uuid> = sqlx::query_scalar(
        "update routine_reports
            set dismissed_at=clock_timestamp(),dismiss_reason='replaced',
                updated_at=clock_timestamp()
          where schedule_id=$1 and conversation_id=$2 and dismissed_at is null
          returning id",
    )
    .bind(card.schedule_id)
    .bind(card.conversation_id)
    .fetch_all(&mut *conn)
    .await?;
    for id in replaced {
        emit_dismissed(
            conn,
            card.conversation_id,
            id,
            card.schedule_id,
            "replaced",
            correlation_id,
        )
        .await?;
    }
    let id = Uuid::now_v7();
    let completed = card.state != ReportState::Pending;
    let row: CardRow = sqlx::query_as(
        "insert into routine_reports(id,owner_id,conversation_id,schedule_id,run_id,copy_id,
            title,state,error,completed_at)
         values($1,$2,$3,$4,$5,$6,$7,$8,$9,case when $10 then clock_timestamp() end)
         returning *",
    )
    .bind(id)
    .bind(card.owner_id)
    .bind(card.conversation_id)
    .bind(card.schedule_id)
    .bind(card.run_id)
    .bind(card.copy_id)
    .bind(card.title)
    .bind(card.state.as_str())
    .bind(card.error)
    .bind(completed)
    .fetch_one(&mut *conn)
    .await?;
    emit_changed(conn, &row.view()?, correlation_id).await?;
    Ok(id)
}

/// Запуск расписания не дошёл до отчёта: у него уже есть карточка (копия упала) или нет
/// (ошибка до копии). В обоих случаях в ленте остаётся карточка с ошибкой.
pub async fn record_failure_in_tx(
    conn: &mut PgConnection,
    schedule: &schedules::Schedule,
    conversation_id: Uuid,
    run: &schedules::ScheduleRun,
    reason: &str,
    correlation_id: Uuid,
) -> Result<(), sqlx::Error> {
    let existing: Option<CardRow> =
        sqlx::query_as("select * from routine_reports where run_id=$1 for update")
            .bind(run.id)
            .fetch_optional(&mut *conn)
            .await?;
    match existing {
        Some(card) => {
            let row: CardRow = sqlx::query_as(
                "update routine_reports
                    set state='failed',error=$2,completed_at=clock_timestamp(),
                        updated_at=clock_timestamp()
                  where id=$1 returning *",
            )
            .bind(card.id)
            .bind(reason)
            .fetch_one(&mut *conn)
            .await?;
            if row.dismissed_at.is_none() {
                emit_changed(conn, &row.view()?, correlation_id).await?;
            }
        }
        None => {
            // Разговор могли удалить, пока шёл приём: карточке негде лежать.
            let present: bool =
                sqlx::query_scalar("select exists(select 1 from conversations where id=$1)")
                    .bind(conversation_id)
                    .fetch_one(&mut *conn)
                    .await?;
            if !present {
                return Ok(());
            }
            let title =
                schedules::render_title(&schedule.title_template, &schedule.name, run.scheduled_at);
            open_in_tx(
                conn,
                NewCard {
                    owner_id: schedule.owner_id,
                    conversation_id,
                    schedule_id: schedule.id,
                    run_id: run.id,
                    copy_id: None,
                    title: &title,
                    state: ReportState::Failed,
                    error: Some(reason),
                },
                correlation_id,
            )
            .await?;
        }
    }
    Ok(())
}

/// Открытые карточки разговора для снимка ленты: у каждого расписания не больше одной.
pub async fn open_for_conversation(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Vec<RoutineReportView>, sqlx::Error> {
    let rows: Vec<CardRow> = sqlx::query_as(
        "select * from routine_reports where conversation_id=$1 and dismissed_at is null
          order by created_at,id",
    )
    .bind(conversation_id)
    .fetch_all(conn)
    .await?;
    rows.iter().map(CardRow::view).collect()
}

#[derive(Debug)]
pub enum DismissError {
    NotFound,
    Db(sqlx::Error),
}
impl From<sqlx::Error> for DismissError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

/// «Завершить»: карточка уходит из ленты у всех клиентов. Повтор безвреден и событий не плодит.
/// Текст отчёта остаётся в истории запусков расписания.
pub async fn dismiss(
    pool: &PgPool,
    owner_id: Uuid,
    conversation_id: Uuid,
    report_id: Uuid,
    correlation_id: Uuid,
) -> Result<(), DismissError> {
    let mut tx = crate::db::begin(pool).await?;
    let card: Option<CardRow> = sqlx::query_as(
        "select * from routine_reports where id=$1 and conversation_id=$2 and owner_id=$3
         for update",
    )
    .bind(report_id)
    .bind(conversation_id)
    .bind(owner_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(card) = card else {
        tx.rollback().await?;
        return Err(DismissError::NotFound);
    };
    if card.dismissed_at.is_none() {
        sqlx::query(
            "update routine_reports
                set dismissed_at=clock_timestamp(),dismiss_reason='owner',
                    updated_at=clock_timestamp()
              where id=$1",
        )
        .bind(card.id)
        .execute(&mut *tx)
        .await?;
        emit_dismissed(
            &mut tx,
            conversation_id,
            card.id,
            card.schedule_id,
            "owner",
            correlation_id,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Исход проверки собирающейся копии.
enum Verdict {
    Wait,
    /// Первый раз увидели начатый ход: от этого момента идёт предел в 60 минут.
    Started,
    Ready(String),
    Failed(&'static str),
}

/// Проход по собирающимся карточкам и копиям, которые пора убрать.
pub async fn sweep(pool: &PgPool, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    let pending: Vec<Uuid> = sqlx::query_scalar(
        "select id from routine_reports where state='pending' order by created_at,id limit $1",
    )
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    for id in pending {
        if let Err(error) = collect_one(pool, id, now).await {
            tracing::warn!(
                kind = "routine_collect_failed",
                report_id = %id,
                error = %crate::dberr::describe(&error),
                "routine report collection failed; retried on the next pass"
            );
        }
    }
    remove_copies(pool).await
}

async fn collect_one(pool: &PgPool, id: Uuid, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    let schedule_id: Option<Uuid> =
        sqlx::query_scalar("select schedule_id from routine_reports where id=$1")
            .bind(id)
            .fetch_optional(pool)
            .await?;
    let Some(schedule_id) = schedule_id else {
        return Ok(());
    };
    let mut tx = crate::db::begin(pool).await?;
    // Порядок замков как у приёма запуска: расписание, затем карточка и запуск.
    let schedule: Option<(Uuid,)> =
        sqlx::query_as("select owner_id from schedules where id=$1 for update skip locked")
            .bind(schedule_id)
            .fetch_optional(&mut *tx)
            .await?;
    if schedule.is_none() {
        tx.rollback().await?;
        return Ok(());
    }
    let card: Option<CardRow> =
        sqlx::query_as("select * from routine_reports where id=$1 and state='pending' for update")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(card) = card else {
        tx.rollback().await?;
        return Ok(());
    };
    match judge(&mut tx, &card, now).await? {
        Verdict::Wait => {
            tx.rollback().await?;
        }
        Verdict::Started => {
            sqlx::query("update routine_reports set turn_started_at=$2 where id=$1")
                .bind(card.id)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
        }
        Verdict::Ready(text) => {
            finish_ready(&mut tx, &card, text).await?;
            tx.commit().await?;
        }
        Verdict::Failed(reason) => {
            finish_failed(&mut tx, &card, reason).await?;
            tx.commit().await?;
        }
    }
    Ok(())
}

async fn judge(
    conn: &mut PgConnection,
    card: &CardRow,
    now: DateTime<Utc>,
) -> Result<Verdict, sqlx::Error> {
    let Some(copy_id) = card.copy_id else {
        return Ok(Verdict::Failed("copy_lost"));
    };
    let copy: Option<(Option<DateTime<Utc>>, String, bool)> = sqlx::query_as(
        "select c.trashed_at, q.run_slot,
                q.hold_reasons @> '[{\"reason\":\"side_parent_busy\"}]'::jsonb
           from conversations c join queue_state q on q.conversation_id=c.id
          where c.id=$1",
    )
    .bind(copy_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((trashed_at, run_slot, parent_busy)) = copy else {
        return Ok(Verdict::Failed("copy_lost"));
    };
    if trashed_at.is_some() {
        return Ok(Verdict::Failed("copy_lost"));
    }
    let input: Option<(Uuid, String, i64)> = sqlx::query_as(
        "select id,state,created_seq from inputs where conversation_id=$1
          order by created_seq limit 1",
    )
    .bind(copy_id)
    .fetch_optional(&mut *conn)
    .await?;
    let Some((_, input_state, input_seq)) = input else {
        return Ok(Verdict::Failed("copy_lost"));
    };
    match input_state.as_str() {
        "rejected" | "cancelled" => return Ok(Verdict::Failed("input_rejected")),
        // Ход принят и слот свободен: ход кончился, ответ в блоках копии.
        "accepted" if run_slot == "free" => {
            let query = format!(
                "select content from blocks
                  where conversation_id=$1 and kind='text' and parent_block_id is null
                    and state='final' and btrim(content)<>'' and created_seq>=$2 and {}
                  order by created_seq desc, created_at desc limit 1",
                crate::agents::main_agent_filter("agent")
            );
            let text: Option<String> = sqlx::query_scalar(sqlx::AssertSqlSafe(query))
                .bind(copy_id)
                .bind(input_seq)
                .fetch_optional(&mut *conn)
                .await?;
            return Ok(match text {
                Some(text) => Verdict::Ready(clip(text.trim())),
                None => Verdict::Failed("empty_report"),
            });
        }
        _ => {}
    }
    let live = crate::executions::live(&mut *conn, copy_id)
        .await?
        .is_some();
    let ever_started: bool =
        sqlx::query_scalar("select exists(select 1 from executions where conversation_id=$1)")
            .bind(copy_id)
            .fetch_one(&mut *conn)
            .await?;
    if !live && ever_started {
        return Ok(Verdict::Failed("copy_failed"));
    }
    if input_state == "queued" {
        // Ход не начался: OpenCode ждёт простоя родителя, остальное — очередь копии.
        if now - card.created_at > START_LIMIT {
            return Ok(Verdict::Failed(if parent_busy {
                "parent_busy_timeout"
            } else {
                "copy_not_started"
            }));
        }
        return Ok(Verdict::Wait);
    }
    match card.turn_started_at {
        None => Ok(Verdict::Started),
        Some(started) if now - started > TURN_LIMIT => Ok(Verdict::Failed("turn_timeout")),
        Some(_) => Ok(Verdict::Wait),
    }
}

async fn finish_ready(
    conn: &mut PgConnection,
    card: &CardRow,
    text: String,
) -> Result<(), sqlx::Error> {
    let row: CardRow = sqlx::query_as(
        "update routine_reports
            set state='ready',body=$2,completed_at=clock_timestamp(),updated_at=clock_timestamp()
          where id=$1 returning *",
    )
    .bind(card.id)
    .bind(&text)
    .fetch_one(&mut *conn)
    .await?;
    let correlation_id = Uuid::now_v7();
    if let Some(run_id) = card.run_id {
        let report = json!({
            "state": "ready",
            "report_id": card.id,
            "title": card.title,
            "text": text,
            "completed_at": row.completed_at,
        });
        sqlx::query(
            "update schedule_runs
                set detail=jsonb_set(detail,'{report}',$2::jsonb),updated_at=clock_timestamp()
              where id=$1",
        )
        .bind(run_id)
        .bind(report)
        .execute(&mut *conn)
        .await?;
        if let Some(run) = schedules::get_run(&mut *conn, card.owner_id, run_id).await? {
            schedules::run_changed(&mut *conn, &run, correlation_id).await?;
        }
    }
    // Закрытая владельцем или вытесненная карточка в ленте не нужна: текст ушёл в историю.
    if row.dismissed_at.is_none() {
        emit_changed(conn, &row.view()?, correlation_id).await?;
        let name: Option<String> = sqlx::query_scalar("select name from schedules where id=$1")
            .bind(card.schedule_id)
            .fetch_optional(&mut *conn)
            .await?;
        crate::notify::on_routine_report_in_tx(
            conn,
            card.conversation_id,
            card.id,
            name.as_deref().unwrap_or_default(),
        )
        .await?;
    }
    Ok(())
}

async fn finish_failed(
    conn: &mut PgConnection,
    card: &CardRow,
    reason: &'static str,
) -> Result<(), sqlx::Error> {
    let correlation_id = Uuid::now_v7();
    let row: CardRow = sqlx::query_as(
        "update routine_reports
            set state='failed',error=$2,completed_at=clock_timestamp(),updated_at=clock_timestamp()
          where id=$1 returning *",
    )
    .bind(card.id)
    .bind(reason)
    .fetch_one(&mut *conn)
    .await?;
    if row.dismissed_at.is_none() {
        emit_changed(conn, &row.view()?, correlation_id).await?;
    }
    let Some(run_id) = card.run_id else {
        return Ok(());
    };
    let Some(run) = schedules::get_run(&mut *conn, card.owner_id, run_id).await? else {
        return Ok(());
    };
    let mut detail = run.detail.clone();
    detail["reason"] = json!(reason);
    detail["report"] = json!({"state": "failed", "report_id": card.id, "title": card.title});
    let moved = schedules::transition_run(
        &mut *conn,
        run.id,
        RunState::Started,
        run.attempt_id,
        RunState::Failed,
        detail.clone(),
        None,
        None,
        None,
        None,
    )
    .await?;
    if !moved {
        return Ok(());
    }
    let mut failed = run.clone();
    failed.state = RunState::Failed;
    failed.detail = detail;
    schedules::run_changed(&mut *conn, &failed, correlation_id).await?;
    schedules::finalize_manual_key(&mut *conn, card.owner_id, &failed).await?;
    let name: Option<String> = sqlx::query_scalar("select name from schedules where id=$1")
        .bind(card.schedule_id)
        .fetch_optional(&mut *conn)
        .await?;
    crate::notify::enqueue_schedule_failure_in_tx(
        conn,
        card.schedule_id,
        name.as_deref().unwrap_or_default(),
        reason,
    )
    .await?;
    Ok(())
}

/// Копии готовых и неудавшихся карточек: остановить и удалить обычным путём бокового чата.
/// Копия, которая ещё отвечает, получает команду остановки, строки уберёт следующий проход.
async fn remove_copies(pool: &PgPool) -> Result<(), sqlx::Error> {
    let rows: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "select id, owner_id, copy_id from routine_reports
          where copy_id is not null and state<>'pending'
          order by updated_at,id limit $1",
    )
    .bind(BATCH)
    .fetch_all(pool)
    .await?;
    for (card_id, owner_id, copy_id) in rows {
        let mut tx = crate::db::begin(pool).await?;
        let exists: bool =
            sqlx::query_scalar("select exists(select 1 from conversations where id=$1 for update)")
                .bind(copy_id)
                .fetch_one(&mut *tx)
                .await?;
        let gone = if exists {
            match crate::side::stop_and_delete_in_tx(&mut tx, owner_id, copy_id, Uuid::now_v7())
                .await
            {
                Ok(done) => done,
                Err(crate::deletion::DeleteError::NotFound) => true,
                Err(crate::deletion::DeleteError::Db(error)) => {
                    let _ = tx.rollback().await;
                    return Err(error);
                }
                Err(error) => {
                    tracing::warn!(kind = "routine_copy_delete_failed", copy_id = %copy_id, error = ?error, "report copy was not deleted");
                    let _ = tx.rollback().await;
                    continue;
                }
            }
        } else {
            true
        };
        if gone {
            sqlx::query("update routine_reports set copy_id=null where id=$1")
                .bind(card_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
    }
    Ok(())
}

/// Уборка: через 30 дней текст отчёта уходит из истории запусков (сами запуски живут по
/// общему правилу уборки), а закрытые карточки удаляются.
pub async fn prune(conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
    let texts = sqlx::query(
        "update schedule_runs set detail=detail #- '{report,text}'
          where detail #> '{report,text}' is not null
            and created_at < clock_timestamp() - $1 * interval '1 day'",
    )
    .bind(TEXT_RETENTION_DAYS as f64)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    let cards = sqlx::query(
        "delete from routine_reports
          where dismissed_at is not null and copy_id is null
            and dismissed_at < clock_timestamp() - $1 * interval '1 day'",
    )
    .bind(TEXT_RETENTION_DAYS as f64)
    .execute(conn)
    .await?
    .rows_affected();
    Ok(texts + cards)
}
