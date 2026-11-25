//! Пуши владельцу через Gotify.
//!
//! Событие ставит строку в `owner_pushes` в своей же транзакции: вставка дешёвая и ничего
//! не ждёт по сети. Отправляет фоновая служба после commit, одна на базу (`singleton`),
//! с коротким сроком запроса, ограниченным числом повторов и общим пределом в минуту.
//! Ключ `dedupe_key` уникален: повторная оценка того же эпизода пуш не повторяет.
//! Строки настроек нет, пока в `hub.toml` не задан `[notify.gotify]`: тогда вставки нет.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use chrono::{DateTime, FixedOffset, SecondsFormat, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use tokio::{sync::watch, time::Instant};
use uuid::Uuid;

use crate::{
    activity::{Evidence, OWNER_LOST},
    config::{GotifyConfig, GotifyToken},
    events::Scope,
    presence::Presence,
    session_tools::settle::{SettleFacts, Settled},
};

pub const KIND_WAITING: &str = "waiting";
pub const KIND_TURN_FINISHED: &str = "turn_finished";
pub const KIND_FAILURE: &str = "failure";
pub const KIND_SIGN_IN: &str = "sign_in";
pub const KIND_SIGN_IN_FAILURES: &str = "sign_in_failures";
pub const KIND_SIGN_IN_LOCK: &str = "sign_in_lock";
/// Сеанс заговорил из новой страны (часть 2: геолокация).
pub const KIND_SESSION_COUNTRY: &str = "session_country";
/// Вход Claude Code на машине скоро закончится: один пуш на машину, провайдера и дату.
pub const KIND_CREDENTIAL_EXPIRY: &str = "credential_expiry";
/// Claude Code отказал по входу: один пуш на эпизод, новый — после успешного хода.
pub const KIND_CREDENTIAL_AUTH: &str = "credential_auth";
/// Экран «Сеансы и вход» в PWA: сюда ведут пуши о входе.
pub const SIGN_IN_PATH: &str = "/settings/sign-in";
/// Запуск расписания провалился: один пуш на расписание в катящийся час.
/// Сессии обычно нет (ошибка до dispatch), поэтому разговор не привязывается.
pub const KIND_SCHEDULE_FAILED: &str = "schedule_failed";
/// Отчёт рутины готов: один пуш на карточку, привязан к родителю.
pub const KIND_ROUTINE_REPORT: &str = "routine_report";
/// Деньги и облачные серверы: лишний сервер удалён, сервер не удаляется, окно просрочено,
/// лишний расход после простоя хаба. Без разговора; повторы разведены ключом с номером слота.
pub const KIND_CLOUD_ALERT: &str = "cloud_alert";
/// Сколько отметка отправки считается живой: дольше строка брошена погибшим хабом.
const SENDING_GRACE_SECS: f64 = 180.0;
/// Катящееся окно троттлинга пушей о несработавших расписаниях.
const SCHEDULE_FAILURE_WINDOW: TimeDelta = TimeDelta::hours(1);
/// Сводный пуш о неудачах шлётся, когда их за час набралось столько.
const SIGN_IN_FAILURE_THRESHOLD: i64 = 5;

pub const PRIORITY_WAITING: u8 = 8;
pub const PRIORITY_FAILURE: u8 = 7;
pub const PRIORITY_TURN_FINISHED: u8 = 5;
pub const PRIORITY_SIGN_IN: u8 = 8;
/// Отказ входа требует действия на машине: важнее предупреждения о скором конце входа.
pub const PRIORITY_CREDENTIAL_AUTH: u8 = 8;
pub const PRIORITY_CREDENTIAL_EXPIRY: u8 = 7;

/// За сколько до конца входа предупреждать владельца.
const CREDENTIAL_WARN: TimeDelta = TimeDelta::days(7);

/// Итог старше этого не шлётся: перезапуск хаба переоценивает давно осевшие сессии.
const FRESH: TimeDelta = TimeDelta::minutes(10);
/// Дольше этого позднее событие не делает старый ход свежим: иначе событие после
/// перезапуска воркера воскрешает пуш о ходе, кончившемся дни назад. Меньше срока
/// хранения строки дедупликации, чтобы в этом окне повтор не прошёл.
const TURN_FINISHED_EXTENSION: TimeDelta = TimeDelta::hours(24);
/// Пуш, не ушедший за это время (лимит, долгий сбой сервера), теряет смысл.
const STALE: TimeDelta = TimeDelta::minutes(30);
/// Сколько строк забирает один проход.
const BATCH: i64 = 20;
/// Открытый чат откладывает пуш на минуту: телефон мог уснуть, не прислав «скрыто».
const VISIBLE_RETRY_SECS: i64 = 60;
/// Больше двух отсрочек не ждём: после них пуш уходит, даже если чат всё ещё виден.
const VISIBLE_RETRIES: i32 = 2;
/// Готовые строки живут неделю: ключи итогов с отметкой времени не повторяются и так.
const RETENTION: TimeDelta = TimeDelta::days(7);
const POLL: Duration = Duration::from_secs(1);
const TITLE_CHARS: usize = 60;
const ANSWER_CHARS: usize = 300;
const DETAIL_CHARS: usize = 200;

/// Какие события ставятся в очередь. Хранится в `push_settings`, чтобы постановка
/// в транзакции проектора знала о выключателях без доступа к конфигу.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushSettings {
    pub waiting: bool,
    pub turn_finished: bool,
    pub failures: bool,
    pub timezone: String,
}

impl From<&GotifyConfig> for PushSettings {
    fn from(config: &GotifyConfig) -> Self {
        Self {
            waiting: config.waiting,
            turn_finished: config.turn_finished,
            failures: config.failures,
            timezone: config.timezone.clone(),
        }
    }
}

/// Старт хаба: включает пуши по конфигу или выключает их и отменяет ожидающие.
pub async fn configure(pool: &PgPool, settings: Option<&PushSettings>) -> Result<(), &'static str> {
    let Some(settings) = settings else {
        let mut tx = crate::db::begin(pool).await.map_err(|_| "db_error")?;
        sqlx::query("delete from push_settings")
            .execute(&mut *tx)
            .await
            .map_err(|_| "db_error")?;
        sqlx::query(
            "update owner_pushes set state='cancelled',updated_at=clock_timestamp()
             where state='pending'",
        )
        .execute(&mut *tx)
        .await
        .map_err(|_| "db_error")?;
        // Уборка не должна зависеть от работающей службы пушей: без таблицы она
        // не запускается, и готовые строки иначе оставались бы до удаления сессий.
        sqlx::query(
            "delete from owner_pushes where state<>'pending'
               and updated_at<clock_timestamp()-$1*interval '1 second'",
        )
        .bind(RETENTION.num_seconds() as f64)
        .execute(&mut *tx)
        .await
        .map_err(|_| "db_error")?;
        return tx.commit().await.map_err(|_| "db_error");
    };
    let known: bool =
        sqlx::query_scalar("select exists(select 1 from pg_timezone_names where name=$1)")
            .bind(&settings.timezone)
            .fetch_one(pool)
            .await
            .map_err(|_| "db_error")?;
    if !known {
        return Err("notify_timezone_invalid");
    }
    sqlx::query(
        "insert into push_settings(singleton,waiting,turn_finished,failures,timezone)
         values(true,$1,$2,$3,$4)
         on conflict(singleton) do update set waiting=excluded.waiting,
           turn_finished=excluded.turn_finished,failures=excluded.failures,
           timezone=excluded.timezone,updated_at=clock_timestamp()",
    )
    .bind(settings.waiting)
    .bind(settings.turn_finished)
    .bind(settings.failures)
    .bind(&settings.timezone)
    .execute(pool)
    .await
    .map_err(|_| "db_error")?;
    Ok(())
}

/// Вставка строки, если пуши включены, событие выбрано и сессия подходит.
/// `root_only`: итоги и сбои делегатов уже доходят до их оркестратора.
/// `revive_cancelled`: отменённый или брошенный пуш возрождается — так повторно
/// открытый запрос получает пуш, которого у него ещё не было.
async fn enqueue_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    kind: &str,
    dedupe_key: &str,
    payload: &Value,
    root_only: bool,
    revive_cancelled: bool,
) -> Result<(), sqlx::Error> {
    let conflict = if revive_cancelled {
        // Возрождённый запрос получает свежий возраст: старую строку иначе бросят
        // как протухшую до первой попытки. `created_at` читает только текст итога
        // хода, а возрождаются лишь ожидающие запросы.
        "on conflict(dedupe_key) do update
           set state='pending', run_at=clock_timestamp(), attempts=0,
               created_at=clock_timestamp(), updated_at=clock_timestamp()
           where owner_pushes.state in ('cancelled','dropped')"
    } else {
        "on conflict(dedupe_key) do nothing"
    };
    // «Без уведомлений» стоит на сессии или на любом её предке: оркестратор
    // выключает пуши своих делегатов, делегат — только свои.
    let sql = format!(
        "with recursive lineage(id,parent_conversation_id,notifications_muted) as (
           select id,parent_conversation_id,notifications_muted
             from conversations where id=$2
           union all
           select p.id,p.parent_conversation_id,p.notifications_muted
             from conversations p join lineage l on p.id=l.parent_conversation_id
         )
         insert into owner_pushes(id,conversation_id,kind,dedupe_key,payload,run_at)
         select $1,c.id,$3,$4,$5,clock_timestamp()
         from push_settings s join conversations c on c.id=$2
         where c.trashed_at is null
           and c.side_of is null
           and (not $6 or c.parent_conversation_id is null)
           and not exists(select 1 from lineage where notifications_muted)
           and ($3 <> 'turn_finished' or c.snoozed_until is null or c.snoozed_until <= clock_timestamp())
           and case $3 when 'waiting' then s.waiting
                       when 'turn_finished' then s.turn_finished
                       when 'routine_report' then s.turn_finished
                       else s.failures end
         {conflict}"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(Uuid::now_v7())
        .bind(conversation_id)
        .bind(kind)
        .bind(dedupe_key)
        .bind(payload)
        .bind(root_only)
        .execute(conn)
        .await?;
    Ok(())
}

/// Открыт запрос к владельцу: вопрос, разрешение, план или одобрение запуска.
/// Любая сессия, корневая или делегат. Один пуш на запрос: повторное открытие
/// того же запроса после возврата исполнения не шлёт снова.
pub async fn on_request_opened_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    request_id: Uuid,
) -> Result<(), sqlx::Error> {
    // Место запроса в ленте: по нему проверяем, прочитал ли владелец карточку.
    // Отсутствующая строка не повод не ставить пуш: тогда просто нет ссылки на ленту.
    let seq: Option<i64> = sqlx::query_scalar("select created_seq from requests where id=$1")
        .bind(request_id)
        .fetch_optional(&mut *conn)
        .await?
        .flatten();
    let mut payload = json!({"request_id": request_id});
    if let Some(seq) = seq {
        payload["seq"] = json!(seq);
    }
    enqueue_in_tx(
        conn,
        conversation_id,
        KIND_WAITING,
        &format!("{KIND_WAITING}:{request_id}"),
        &payload,
        false,
        true,
    )
    .await
}

/// Отчёт рутины собран: «{имя} готов». Ключ — карточка, повтор не шлёт второй пуш.
/// Выключатель тот же, что у пуша о конце хода: отдельной настройки у рутин нет.
pub async fn on_routine_report_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    report_id: Uuid,
    schedule_name: &str,
) -> Result<(), sqlx::Error> {
    enqueue_in_tx(
        conn,
        conversation_id,
        KIND_ROUTINE_REPORT,
        &format!("{KIND_ROUTINE_REPORT}:{report_id}"),
        &json!({"report_id": report_id, "schedule_name": schedule_name}),
        false,
        false,
    )
    .await
}

/// Ход завис в генерации: единственный путь пуша о зависании. Ключ называет разговор и
/// ход, поэтому зависание следующего хода того же исполнения не глушится, а второе
/// зависание того же хода получает свой пуш. Обычный путь `stalled` для verdict
/// «завис» пуш не ставит.
pub async fn on_hung_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    turn_id: Uuid,
    second: bool,
) -> Result<(), sqlx::Error> {
    // Общий `stalled` этой же непрерывной тишины уже дошёл до владельца раньше:
    // карточка остаётся, второго пуша на одну тишину нет.
    if same_generic_silence(conn, conversation_id, turn_id).await? {
        return Ok(());
    }
    enqueue_in_tx(
        conn,
        conversation_id,
        KIND_FAILURE,
        &format!(
            "hung:{conversation_id}:{turn_id}:{}",
            if second { "second" } else { "first" }
        ),
        &json!({"state": "hung", "evidence": {"kind": "hung", "second": second}}),
        false,
        false,
    )
    .await
}

/// Агент просит секрет: пуш говорит, что именно нужно владельцу. Значение в
/// пуш не попадает — его ещё нет, а имя пришло из аргументов инструмента.
pub async fn on_secret_request_opened_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    secret_request_id: Uuid,
) -> Result<(), sqlx::Error> {
    enqueue_in_tx(
        conn,
        conversation_id,
        KIND_WAITING,
        &format!("{KIND_WAITING}:secret:{secret_request_id}"),
        &json!({"secret_request_id": secret_request_id}),
        false,
        true,
    )
    .await
}

/// Осевшее состояние корневой сессии после шлюза автопинка — то же решение, что у
/// подписок. Ключ итога хода — конец хода: дребезг состояния меняет отпечаток
/// оседания, но не сам ход, и второго пуша о нём быть не должно.
pub async fn on_settled_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    state: Settled,
    fingerprint: &str,
    facts: &SettleFacts,
    evidence: &Evidence,
) -> Result<(), sqlx::Error> {
    // Вход Claude Code: отказ и успешный ход двигают эпизод пуша даже при выключенных
    // пушах — иначе включённые позже пуши молчали бы об уже случившемся отказе.
    if let Some((machine_id, harness)) = sqlx::query_as::<_, (Uuid, String)>(
        "select machine_id,harness from conversations where id=$1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?
        && canterlot_proto::harness::provider_of(&harness) == Some("claude")
    {
        match (state, evidence) {
            (Settled::Failed, Evidence::Failed { reason }) if reason == "auth_required" => {
                credential_auth_required_in_tx(conn, machine_id).await?;
            }
            (
                Settled::Idle,
                Evidence::Idle {
                    last_turn_at: Some(_),
                    ..
                },
            ) => {
                credential_auth_ok_in_tx(conn, machine_id).await?;
            }
            _ => {}
        }
    }
    let turn_from_owner: bool;
    let owner_mark: Option<DateTime<Utc>>;
    let mut final_mark = None;
    let (kind, turn) = match state {
        // Ход должен быть и быть недавним: новая пустая сессия и давно осевшая
        // после перезапуска хаба пуша не дают. Свежесть — от последнего события:
        // сессию, которую держала фоновая задача, пуш обязан догнать после её конца.
        Settled::Idle => match facts.last_turn_at {
            Some(turn)
                if facts.now - turn <= FRESH
                    || (facts.now - turn <= TURN_FINISHED_EXTENSION
                        && facts
                            .last_event_at
                            .is_some_and(|at| facts.now - at <= FRESH)) =>
            {
                turn_from_owner = last_turn_from_owner(conn, conversation_id).await?;
                owner_mark = owner_waiting_since(conn, conversation_id).await?;
                if !turn_from_owner {
                    // Ход от события сам по себе молчит. Единственный пуш — итог
                    // запроса владельца, когда ни один делегат больше не работает.
                    let Some(mark) = owner_mark else {
                        return Ok(());
                    };
                    if delegate_work_remains(conn, conversation_id).await? {
                        return Ok(());
                    }
                    final_mark = Some(mark);
                }
                (KIND_TURN_FINISHED, Some(turn))
            }
            _ => return Ok(()),
        },
        Settled::Failed | Settled::OwnerLost | Settled::Stalled | Settled::QuotaPaused => {
            if facts.now - facts.since > FRESH {
                return Ok(());
            }
            // Зависание в генерации уведомляет спасение хода: у него ключ по ходу и текст
            // про зависание, второй пуш «застряла» об одном и том же не нужен.
            if matches!(
                evidence,
                Evidence::Stalled {
                    hung: Some(true),
                    ..
                }
            ) {
                return Ok(());
            }
            // Тот же непрерывный молчащий ход уже получил пуш о зависании: возврат связи
            // и греющееся окно наблюдения не должны добавлять второй, общий.
            if same_hang_silence(conn, conversation_id, facts.last_event_at).await? {
                return Ok(());
            }
            // Сбой пушится один раз на эпизод: дребезг состояния внутри того же
            // исполнения и причины не повторяет пуш.
            let silence = if state == Settled::Stalled {
                silence_scope(conn, conversation_id, facts.last_event_at).await?
            } else {
                None
            };
            enqueue_failure_in_tx(conn, conversation_id, state, fingerprint, evidence, silence)
                .await?;
            return Ok(());
        }
        Settled::WaitingForUser | Settled::Stopped => return Ok(()),
    };
    let key = match final_mark {
        // Один итог на запрос владельца: отметка «владелец ждёт» и есть ключ эпизода.
        Some(mark) => format!(
            "{kind}:all_done:{conversation_id}:{}",
            mark.to_rfc3339_opts(SecondsFormat::Micros, true)
        ),
        None => match turn {
            Some(turn) => format!(
                "{kind}:{conversation_id}:{}",
                turn.to_rfc3339_opts(SecondsFormat::Micros, true)
            ),
            None => format!("{kind}:{conversation_id}:{fingerprint}"),
        },
    };
    let evidence = serde_json::to_value(evidence).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let mut payload = json!({"state": state_name(state), "evidence": evidence});
    if final_mark.is_some() {
        payload["final"] = json!(true);
    }
    // Место последнего блока хода в ленте: если владелец дочитал до него, итог он видел.
    if let (KIND_TURN_FINISHED, Some(turn)) = (kind, turn) {
        let seq: Option<i64> = sqlx::query_scalar(
            "select max(b.created_seq) from blocks b
             where b.conversation_id=$1 and b.created_at <= $2
               and b.created_at >= coalesce((select started_at from runs
                   where conversation_id=$1 order by finished_at desc limit 1), $2)",
        )
        .bind(conversation_id)
        .bind(turn)
        .fetch_one(&mut *conn)
        .await?;
        if let Some(seq) = seq {
            payload["seq"] = json!(seq);
        }
    }
    enqueue_in_tx(conn, conversation_id, kind, &key, &payload, true, false).await?;
    if let Some(mark) = final_mark {
        clear_owner_waiting(conn, conversation_id, Some(mark)).await?;
    } else if turn_from_owner && !delegate_work_remains(conn, conversation_id).await? {
        // Пуш о ходе владельца уже покрывает запрос: второго «Всё готово» нет.
        clear_owner_waiting(conn, conversation_id, owner_mark).await?;
    }
    Ok(())
}

/// Пуш общего `stalled` для того же хода и той же тишины: он записал ход и отметку
/// последнего прогресса (`silence`), а отметка эпизода зависания не новее неё,
/// то есть между ними прогресса потока не было.
async fn same_generic_silence(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    turn_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(
            select 1 from owner_pushes p
            join hung_rescues h on h.conversation_id=p.conversation_id
            where p.conversation_id=$1 and p.kind=$3
              and p.payload#>>'{silence,turn}'=$2::text
              and h.progress_mark is not null
              and h.progress_mark <= (p.payload#>>'{silence,at}')::timestamptz)",
    )
    .bind(conversation_id)
    .bind(turn_id)
    .bind(KIND_FAILURE)
    .fetch_one(&mut *conn)
    .await
}

/// Открытый эпизод зависания текущего хода, а прогресса потока после него не было:
/// это та же тишина, о которой владельцу уже сказал пуш зависания.
async fn same_hang_silence(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    last_event_at: Option<DateTime<Utc>>,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists(
            select 1 from hung_rescues h
            join queue_state q on q.conversation_id=h.conversation_id and q.run_slot='running'
            join inputs i on i.id=q.run_slot_input_id
            where h.conversation_id=$1 and h.episode_active and h.turn_id=i.command_id
              and h.progress_mark is not null
              and ($2::timestamptz is null or $2 <= h.progress_mark))",
    )
    .bind(conversation_id)
    .bind(last_event_at)
    .fetch_one(&mut *conn)
    .await
}

/// Ход и отметка прогресса, к которым относится общий `stalled`: по ним пуш зависания
/// того же хода узнаёт, что владелец о этой тишине уже услышал.
async fn silence_scope(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    last_event_at: Option<DateTime<Utc>>,
) -> Result<Option<Value>, sqlx::Error> {
    let Some(at) = last_event_at else {
        return Ok(None);
    };
    let turn: Option<Uuid> = sqlx::query_scalar(
        "select i.command_id from queue_state q join inputs i on i.id=q.run_slot_input_id
         where q.conversation_id=$1 and q.run_slot='running'",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(turn
        .map(|turn| json!({"turn": turn, "at": at.to_rfc3339_opts(SecondsFormat::Micros, true)})))
}

/// Пуш о сбое привязан к эпизоду: исполнение, состояние и причина. Внутри эпизода
/// пуш ставится один раз; если владелец прочитал сессию после первой постановки,
/// новая постановка не делается. Новое исполнение, состояние или причина — новый эпизод.
async fn enqueue_failure_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    state: Settled,
    fingerprint: &str,
    evidence: &Evidence,
    silence: Option<Value>,
) -> Result<(), sqlx::Error> {
    let execution: Option<Uuid> = sqlx::query_scalar(
        "select id from executions where conversation_id=$1
         order by generation desc limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    let episode = match execution {
        Some(execution) => format!("{execution}:{}", failure_episode(state, evidence)),
        // Синтетическая строка без исполнения: отпечаток не даёт ей писать заново.
        None => format!("no_execution:{fingerprint}"),
    };
    let stored: Option<(bool, i64)> = sqlx::query_as(
        "select pushed,read_seq from failure_push_episodes
         where conversation_id=$1 and episode=$2",
    )
    .bind(conversation_id)
    .bind(&episode)
    .fetch_optional(&mut *conn)
    .await?;
    let last_read: i64 = sqlx::query_scalar("select last_read_seq from conversations where id=$1")
        .bind(conversation_id)
        .fetch_one(&mut *conn)
        .await?;
    match stored {
        Some((pushed, read_seq)) => {
            // Тот же эпизод: либо пуш уже был, либо владелец прочитал после него.
            // Непоставленный пуш (пуши были выключены) можно поставить, если не читали.
            if pushed || last_read >= read_seq {
                return Ok(());
            }
        }
        None => {
            let read_seq: i64 = sqlx::query_scalar(
                "select coalesce((select last_seq from scope_counters where scope=$1),0)",
            )
            .bind(
                Scope::Conversation {
                    id: conversation_id,
                }
                .key(),
            )
            .fetch_one(&mut *conn)
            .await?;
            sqlx::query(
                "insert into failure_push_episodes(conversation_id,episode,read_seq,pushed)
                 values($1,$2,$3,false)
                 on conflict(conversation_id,episode) do nothing",
            )
            .bind(conversation_id)
            .bind(&episode)
            .bind(read_seq)
            .execute(&mut *conn)
            .await?;
        }
    }
    let key = format!("{KIND_FAILURE}:{conversation_id}:{episode}");
    let evidence = serde_json::to_value(evidence).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    let mut payload = json!({"state": state_name(state), "evidence": evidence});
    if let Some(silence) = silence {
        payload["silence"] = silence;
    }
    enqueue_in_tx(
        conn,
        conversation_id,
        KIND_FAILURE,
        &key,
        &payload,
        true,
        false,
    )
    .await?;
    let queued: bool =
        sqlx::query_scalar("select exists(select 1 from owner_pushes where dedupe_key=$1)")
            .bind(&key)
            .fetch_one(&mut *conn)
            .await?;
    if queued {
        sqlx::query(
            "update failure_push_episodes set pushed=true,updated_at=clock_timestamp()
             where conversation_id=$1 and episode=$2",
        )
        .bind(conversation_id)
        .bind(&episode)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

/// Причина эпизода: `failed:<reason>`, `stalled`, `quota_paused:<provider>:<window>`,
/// `owner_lost`. Исполнение добавляется перед ней отдельно.
fn failure_episode(state: Settled, evidence: &Evidence) -> String {
    match state {
        Settled::OwnerLost => OWNER_LOST.to_owned(),
        Settled::Failed => match evidence {
            Evidence::Failed { reason } if reason == OWNER_LOST => OWNER_LOST.to_owned(),
            Evidence::Failed { reason } => format!("failed:{reason}"),
            _ => "failed".to_owned(),
        },
        Settled::Stalled => "stalled".to_owned(),
        Settled::QuotaPaused => match evidence {
            Evidence::QuotaPaused {
                provider, window, ..
            } => format!(
                "quota_paused:{}:{}",
                provider.as_deref().unwrap_or("unknown"),
                window.as_deref().unwrap_or("unknown")
            ),
            _ => "quota_paused".to_owned(),
        },
        _ => state_name(state).to_owned(),
    }
}

/// Последний законченный ход начат вводом владельца. Строка без ввода (старые данные,
/// синтетические тесты) считается владельческой: иначе итог давнего хода пропал бы.
async fn last_turn_from_owner(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let origin: Option<Option<Value>> = sqlx::query_scalar(
        "select i.origin
         from runs r left join inputs i on i.command_id=r.input_command_id
         where r.conversation_id=$1
         order by r.finished_at desc nulls last, r.started_at desc limit 1",
    )
    .bind(conversation_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(origin.flatten().is_none())
}

async fn owner_waiting_since(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar("select owner_waiting_since from conversations where id=$1")
        .bind(conversation_id)
        .fetch_one(&mut *conn)
        .await
}

async fn clear_owner_waiting(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    mark: Option<DateTime<Utc>>,
) -> Result<(), sqlx::Error> {
    let Some(mark) = mark else {
        return Ok(());
    };
    sqlx::query(
        "update conversations set owner_waiting_since=null
         where id=$1 and owner_waiting_since=$2",
    )
    .bind(conversation_id)
    .bind(mark)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Незавершённая работа делегатов: ребёнок (транзитивно) не осел или его ввод ждёт
/// выдачи, либо у этого родителя ещё стоит очередь запусков.
async fn delegate_work_remains(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "with recursive tree(id) as (
           select id from conversations where parent_conversation_id=$1
           union all
           select c.id from conversations c join tree t on c.parent_conversation_id=t.id
         )
         select exists(
           select 1 from tree t join conversations c on c.id=t.id
           left join conversation_derived d on d.conversation_id=t.id
           where c.trashed_at is null
             and (d.conversation_id is null
                  or d.activity_state not in ('idle','failed','stopped')
                  or exists(select 1 from inputs i
                            where i.conversation_id=t.id
                              and i.state in ('queued','dispatched','on_machine'))))
           or exists(select 1 from spawn_queue q
                     where q.parent_id=$1 and q.state in ('queued','starting'))",
    )
    .bind(conversation_id)
    .fetch_one(&mut *conn)
    .await
}

fn state_name(state: Settled) -> &'static str {
    match state {
        Settled::OwnerLost => OWNER_LOST,
        other => other.as_str(),
    }
}

/// Корзина: ожидающие пуши сессии больше не нужны. Окончательная уборка удаляет строки.
pub async fn cancel_for_conversation_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update owner_pushes set state='cancelled',updated_at=clock_timestamp()
         where conversation_id=$1 and state='pending'",
    )
    .bind(conversation_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Деньги: важнее обычного предупреждения, как отказ входа.
pub const PRIORITY_CLOUD_ALERT: u8 = 8;

/// Копейки как «123,45 ₽» для текста пуша.
pub fn kopecks_text(kopecks: i64) -> String {
    let sign = if kopecks < 0 { "-" } else { "" };
    let abs = kopecks.unsigned_abs();
    format!("{sign}{},{:02} ₽", abs / 100, abs % 100)
}

/// Момент по Москве для текста пуша: «24.11 18:40 МСК». Москва без перевода часов.
fn moscow_moment(at: Option<&str>) -> String {
    let Some(text) = at else {
        return "?".to_owned();
    };
    let Ok(parsed) = DateTime::parse_from_rfc3339(text) else {
        return clip(text, TITLE_CHARS);
    };
    let moscow = FixedOffset::east_opt(3 * 3600).expect("a valid fixed offset");
    parsed
        .with_timezone(&moscow)
        .format("%d.%m %H:%M МСК")
        .to_string()
}

/// Причина, по которой замены нет, по-русски.
fn replacement_reason(reason: &str) -> &'static str {
    match reason {
        "budget" => "бюджет",
        "no_time" => "мало времени до предела",
        "no_waiting_job" => "нет задач в очереди",
        "window_exists" => "окно уже есть",
        "auto_off" => "автоматический режим выключен",
        _ => "цена у провайдера недоступна",
    }
}

fn cloud_alert_text(payload: &Value) -> Option<(String, String)> {
    let code = payload["code"].as_str()?;
    let name = clip(payload["name"].as_str().unwrap_or("?"), TITLE_CHARS);
    let money = |key: &str| payload[key].as_i64().map(kopecks_text);
    Some(match code {
        "budget_80" | "budget_100" => {
            let percent = payload["percent"].as_i64().unwrap_or(80);
            let spent = money("spent_kopecks").unwrap_or_else(|| "?".to_owned());
            let budget = money("budget_kopecks").unwrap_or_else(|| "?".to_owned());
            (
                format!("Облако: достигнуто {percent}% бюджета"),
                format!(
                    "Расход за месяц {} из {} — {percent}% месячного бюджета облака.",
                    spent, budget
                ),
            )
        }
        "auto_started" => (
            "Облачный раннер запущен автоматически".to_owned(),
            format!(
                "Зарезервировано {} на ожидающую работу. Окно живёт до жёсткого предела.",
                money("reservation_kopecks").unwrap_or_else(|| "?".to_owned())
            ),
        ),
        "stray_server_deleted" => (
            "Облако: удалена лишняя машина".to_owned(),
            format!("Машина {name} в каталоге раннеров не принадлежала ни одному окну; удалена."),
        ),
        "stray_server_stuck" => (
            "Облако: лишняя машина не удаляется".to_owned(),
            format!(
                "Машина {name} не принадлежит окну и не удаляется: она продолжает тарифицироваться."
            ),
        ),
        "stray_disk_deleted" => (
            "Облако: удалён лишний диск".to_owned(),
            format!("Диск {name} в каталоге раннеров остался без машины и окна; удалён."),
        ),
        "stray_disk_stuck" => (
            "Облако: лишний диск не удаляется".to_owned(),
            format!(
                "Диск {name} остался без машины и окна и не удаляется: он продолжает \
                 тарифицироваться."
            ),
        ),
        "delete_failing" => (
            "Облако: машина не удаляется".to_owned(),
            format!(
                "Машина {name} не удалилась после нескольких попыток и продолжает тарифицироваться."
            ),
        ),
        "disk_delete_failing" => (
            "Облако: диск не удаляется".to_owned(),
            format!(
                "Диск окна {name} не удалился после нескольких попыток и продолжает \
                 тарифицироваться."
            ),
        ),
        "preempted" => (
            "Облако: Яндекс прервал окно".to_owned(),
            format!("Яндекс остановил прерываемую машину окна {name}; задачи вернутся в очередь."),
        ),
        "replacement_started" => (
            "Облако: поднимаю замену окна".to_owned(),
            format!(
                "Облако: поднимаю замену окна {name} до {}; задачи из очереди перейдут на неё.",
                moscow_moment(payload["deadline"].as_str())
            ),
        ),
        "replacement_skipped" => (
            "Облако: замена окна не создана".to_owned(),
            format!(
                "Замена окна {name} не создана: {}.",
                replacement_reason(payload["reason"].as_str().unwrap_or_default())
            ),
        ),
        "vm_broken" => (
            "Облако: машина окна остановилась".to_owned(),
            format!(
                "Машина окна {name} остановилась или сломалась сама. Машина и диск удаляются, \
                 незаконченные задачи раннера потеряны."
            ),
        ),
        "prepare_failed" => {
            let code = payload["exit_code"].as_i64().unwrap_or(-1);
            (
                format!("Облако: подготовка окна {name} не удалась (код {code}), окно удаляется"),
                format!(
                    "Скрипт подготовки завершился с кодом {code}; машина и диск окна удаляются \
                     обычным сносом."
                ),
            )
        }
        "window_overrun" => (
            "Облако: окно просрочено".to_owned(),
            format!(
                "Окно {name} живёт дольше жёсткого предела с запасами. Удаляю машину.{}",
                money("extra_kopecks")
                    .map(|extra| format!(" Лишний расход около {extra}."))
                    .unwrap_or_default()
            ),
        ),
        "outage_extra_cost" => (
            "Облако: лишний расход после простоя хаба".to_owned(),
            format!(
                "Пока хаб не работал, окно {name} прожило дольше предела.{}",
                money("extra_kopecks")
                    .map(|extra| format!(" Лишний расход около {extra}."))
                    .unwrap_or_default()
            ),
        ),
        "hard_cap" => (
            "Облако: предел окна истёк до подключения".to_owned(),
            format!("Окно {name} не подключилось до жёсткого предела; машина удаляется."),
        ),
        "cloud_window_hard_cap" => (
            "Облако: жёсткий предел, задачи отменены".to_owned(),
            format!("Окно {name} дошло до жёсткого предела; незаконченные задачи отменены."),
        ),
        "ownership_mismatch" => (
            "Облако: ресурс не принадлежит окну".to_owned(),
            format!(
                "{} не совпал по имени или каталогу с окном {name}; удаление остановлено, \
                 проверьте консоль Yandex Cloud.",
                match (payload["instance_id"].as_str(), payload["disk_id"].as_str()) {
                    (Some(id), _) => format!("Машина {}", clip(id, TITLE_CHARS)),
                    (None, Some(id)) => format!("Диск {}", clip(id, TITLE_CHARS)),
                    (None, None) => "Ресурс".to_owned(),
                }
            ),
        ),
        "late_create" => (
            "Облако: поздняя машина возвращена окну".to_owned(),
            format!(
                "Машина окна {name} появилась после неудавшегося создания; она удаляется \
                 обычным путём, итог считается с вызова создания."
            ),
        ),
        "late_disk" => (
            "Облако: диск закрытого окна возвращён окну".to_owned(),
            format!(
                "Диск окна {name} остался в каталоге без машины; он удаляется, а его время \
                 входит в итог окна."
            ),
        ),
        "reprice_growth" => (
            "Облако: цена выросла, окно не создано".to_owned(),
            format!(
                "Новая цена подняла резерв окна {name} до {} выше одобренных {}; машина не \
                 создавалась, расход 0.",
                money("reservation_kopecks").unwrap_or_else(|| "?".to_owned()),
                money("approved_kopecks").unwrap_or_else(|| "?".to_owned())
            ),
        ),
        "window_gone" => (
            "Облако: машина окна исчезла".to_owned(),
            format!("Машины окна {name} нет в каталоге, хотя удаление не запрашивалось."),
        ),
        _ => return None,
    })
}

/// Денежный пуш владельцу: ставится в очередь, если пуши включены. Ключ уникален: повторный
/// вызов с тем же ключом пуш не повторяет; для повторов каждые 30 минут вызывающая сторона
/// кладёт в ключ номер слота.
pub async fn enqueue_cloud_alert(
    pool: &PgPool,
    dedupe_key: &str,
    payload: &Value,
) -> Result<bool, sqlx::Error> {
    // Без Gotify пуша не будет, а деньги идут: запись в журнале остаётся (код и имена, без
    // значений).
    tracing::warn!(
        kind = "cloud_alert",
        code = payload["code"].as_str().unwrap_or("unknown"),
        "owner cloud alert"
    );
    enqueue_auth(pool, KIND_CLOUD_ALERT, dedupe_key, payload).await
}

/// Тот же денежный пуш, но в транзакции вызывающей стороны: метка месяца и строка очереди
/// фиксируются вместе, иначе оповещение теряется после сбоя вставки.
pub(crate) async fn enqueue_cloud_alert_in_tx(
    conn: &mut PgConnection,
    dedupe_key: &str,
    payload: &Value,
) -> Result<bool, sqlx::Error> {
    tracing::warn!(
        kind = "cloud_alert",
        code = payload["code"].as_str().unwrap_or("unknown"),
        "owner cloud alert"
    );
    enqueue_auth_in_tx(conn, KIND_CLOUD_ALERT, dedupe_key, payload).await
}

/// «Без уведомлений»: ожидающие пуши сессии и всех её потомков отменяются.
/// Деревья мелкие, рекурсия дешевле отдельного обхода по одному разговору.
pub async fn cancel_for_conversation_tree_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "with recursive tree(id) as (
           select id from conversations where id=$1
           union all
           select c.id from conversations c join tree t on c.parent_conversation_id=t.id
         )
         update owner_pushes set state='cancelled',updated_at=clock_timestamp()
         where conversation_id in (select id from tree) and state='pending'",
    )
    .bind(conversation_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Пуш о входе: без разговора, только если пуши включены (`push_settings`).
/// Адреса и города уходят только в собственный Gotify владельца.
async fn enqueue_auth(
    pool: &PgPool,
    kind: &str,
    dedupe_key: &str,
    payload: &Value,
) -> Result<bool, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    enqueue_auth_in_tx(&mut conn, kind, dedupe_key, payload).await
}

async fn enqueue_auth_in_tx(
    conn: &mut PgConnection,
    kind: &str,
    dedupe_key: &str,
    payload: &Value,
) -> Result<bool, sqlx::Error> {
    let inserted = sqlx::query(
        "insert into owner_pushes(id,conversation_id,kind,dedupe_key,payload,run_at)
         select $1,null,$2,$3,$4,clock_timestamp() from push_settings
         on conflict(dedupe_key) do nothing",
    )
    .bind(Uuid::now_v7())
    .bind(kind)
    .bind(dedupe_key)
    .bind(payload)
    .execute(conn)
    .await?
    .rows_affected()
        > 0;
    Ok(inserted)
}

/// Несработавший запуск расписания: ожидающие пуши сливаются по расписанию.
/// Постановка пуш не ограничивает (6): метка `last_failure_push_at` двигается
/// при отправке, а не при постановке, и «не чаще раза в час» проверяет
/// отправитель в момент доставки (`Sender::schedule_cooldown`). Если пуш
/// расписания уже ждёт отправки (или повтора), новый исход только обновляет
/// причину в нём. Вызывать только для `failed`.
pub(crate) async fn enqueue_schedule_failure_in_tx(
    conn: &mut PgConnection,
    schedule_id: Uuid,
    schedule_name: &str,
    reason: &str,
) -> Result<bool, sqlx::Error> {
    // Замок расписания: параллельные исходы одного расписания упорядочены.
    // Порядок замков общий с попыткой отправки (`begin_attempt`): сначала строка
    // расписания, затем строка пуша.
    let exists: Option<Uuid> =
        sqlx::query_scalar("select id from schedules where id=$1 for update")
            .bind(schedule_id)
            .fetch_optional(&mut *conn)
            .await?;
    if exists.is_none() {
        return Ok(false);
    }
    let payload = json!({
        "schedule_id": schedule_id,
        "schedule_name": schedule_name,
        "reason": reason,
    });
    let coalesced = sqlx::query(
        // Срок годности считается от последнего слитого исхода: свежий сбой,
        // влившийся в давно ждущий пуш, не должен выбыть как устаревший.
        "update owner_pushes set payload=$2,created_at=clock_timestamp(),
            updated_at=clock_timestamp()
          where kind=$1 and state='pending' and payload->>'schedule_id'=$3",
    )
    .bind(KIND_SCHEDULE_FAILED)
    .bind(&payload)
    .bind(schedule_id.to_string())
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if coalesced > 0 {
        return Ok(false);
    }
    enqueue_auth_in_tx(
        conn,
        KIND_SCHEDULE_FAILED,
        &format!("{KIND_SCHEDULE_FAILED}:{schedule_id}:{}", Uuid::now_v7()),
        &payload,
    )
    .await
}

/// Человекочитаемая причина несработавшего запуска для пуша.
fn schedule_failure_text(reason: &str) -> String {
    match reason {
        "precheck_result_unknown" => "предпроверка не дождалась воркера",
        "precheck_capability_missing" => "воркер не умеет делать предпроверку",
        "precheck_invalid_attempt" => "внутренняя ошибка предпроверки",
        "precheck_attempt_mismatch" => "ответ предпроверки не совпал с попыткой",
        "precheck_error" => "ошибка запуска предпроверки",
        "create_failed" => "не создалась сессия",
        "submit_failed" => "не поставился ввод",
        "conversation_not_found" => "разговор пропал или удалён",
        "database_unavailable" => "база была недоступна",
        "invalid_schedule" => "расписание повреждено",
        "limit_settings_unavailable" | "limits_unavailable" => "не прочитались лимиты",
        "routing_policy_unavailable" => "не прочиталась политика маршрутизации",
        "machine_catalog_unavailable" => "не прочитался каталог машины",
        "whitelist_unavailable" => "не прочитался белый список",
        "live_counts_unavailable" => "не посчиталась занятость",
        "preset_unavailable" => "не разрешился пресет",
        "queue_unavailable" => "не прочиталась очередь",
        "no_native_session" => "у сессии ещё нет записи для отчёта",
        "report_capability_missing" => "воркер машины не умеет боковой чат",
        "copy_failed" | "copy_lost" => "копия сессии не дожила до конца отчёта",
        "turn_timeout" => "копия не закончила отчёт за 60 минут",
        "parent_busy_timeout" | "copy_not_started" => "копия не начала работу за два часа",
        "empty_report" => "копия закончила без текста ответа",
        "input_rejected" => "копия отклонила запрос отчёта",
        other => return format!("внутренняя ошибка ({other})"),
    }
    .to_owned()
}

/// Конец входа Claude Code приблизился: один пуш на машину, провайдера и дату.
/// Повторный вход приносит новую дату и взводит пуш заново. Часы — базы: срок и
/// отметка «уже сказали» сравниваются в одной транзакции.
pub async fn credential_expiry_sweep(pool: &PgPool) -> Result<usize, sqlx::Error> {
    let due: Vec<(Uuid, String, DateTime<Utc>)> = sqlx::query_as(
        "select pc.machine_id,pc.provider,pc.expires_at
           from provider_credentials pc join machines m on m.id=pc.machine_id
          where pc.provider='claude' and pc.expires_at is not null
            and pc.expires_at <= clock_timestamp() + $1 * interval '1 second'
            and (pc.warned_expires_at is null or pc.warned_expires_at <> pc.expires_at)
          order by pc.expires_at",
    )
    .bind(CREDENTIAL_WARN.num_seconds() as f64)
    .fetch_all(pool)
    .await?;
    let mut queued = 0;
    for (machine_id, provider, expires_at) in due {
        let stamp = expires_at.to_rfc3339_opts(SecondsFormat::Micros, true);
        let key = format!("{KIND_CREDENTIAL_EXPIRY}:{machine_id}:{provider}:{stamp}");
        let inserted = enqueue_auth(
            pool,
            KIND_CREDENTIAL_EXPIRY,
            &key,
            &json!({"machine_id": machine_id, "provider": provider, "expires_at": stamp}),
        )
        .await?;
        // Без [notify.gotify] строки нет: отметку не ставим, иначе включённые позже
        // пуши так и не сказали бы о дате.
        if !inserted {
            let exists: bool =
                sqlx::query_scalar("select exists(select 1 from owner_pushes where dedupe_key=$1)")
                    .bind(&key)
                    .fetch_one(pool)
                    .await?;
            if !exists {
                continue;
            }
        }
        sqlx::query(
            "update provider_credentials set warned_expires_at=$3
              where machine_id=$1 and provider=$2 and expires_at=$3",
        )
        .bind(machine_id)
        .bind(&provider)
        .bind(expires_at)
        .execute(pool)
        .await?;
        queued += 1;
    }
    Ok(queued)
}

/// Claude Code отказал по входу: пуш один на эпизод. Новый эпизод открывает только
/// успешный ход на этой машине; дребезг того же отказа второго пуша не даёт.
async fn credential_auth_required_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
) -> Result<bool, sqlx::Error> {
    // Строка входа сериализует два оседания одного отказа в двух разговорах: вставка
    // новой строки ждёт соседа, затем `for update` держит её до commit.
    sqlx::query(
        "insert into provider_credentials(machine_id,provider) values($1,'claude')
         on conflict(machine_id,provider) do nothing",
    )
    .bind(machine_id)
    .execute(&mut *conn)
    .await?;
    let (warned, ok): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) = sqlx::query_as(
        "select auth_warned_at,auth_ok_at from provider_credentials
          where machine_id=$1 and provider='claude' for update",
    )
    .bind(machine_id)
    .fetch_one(&mut *conn)
    .await?;
    if warned.is_some_and(|warned| ok.is_none_or(|ok| ok <= warned)) {
        return Ok(false);
    }
    let now = Utc::now();
    let key = format!(
        "{KIND_CREDENTIAL_AUTH}:{machine_id}:{}",
        now.to_rfc3339_opts(SecondsFormat::Micros, true)
    );
    let inserted = enqueue_auth_in_tx(
        conn,
        KIND_CREDENTIAL_AUTH,
        &key,
        &json!({"machine_id": machine_id}),
    )
    .await?;
    if inserted {
        sqlx::query(
            "update provider_credentials set auth_warned_at=$2
              where machine_id=$1 and provider='claude'",
        )
        .bind(machine_id)
        .bind(now)
        .execute(&mut *conn)
        .await?;
    }
    Ok(inserted)
}

/// Успешный ход Claude на машине: следующий отказ по входу снова достоин пуша.
async fn credential_auth_ok_in_tx(
    conn: &mut PgConnection,
    machine_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update provider_credentials set auth_ok_at=clock_timestamp()
          where machine_id=$1 and provider='claude'",
    )
    .bind(machine_id)
    .execute(conn)
    .await?;
    Ok(())
}

/// Новый сеанс: «Новый вход в canterlot», в тексте устройство, место, адрес и способ.
pub async fn on_sign_in(pool: &PgPool, session_id: Uuid, body: &str) -> Result<(), sqlx::Error> {
    enqueue_auth(
        pool,
        KIND_SIGN_IN,
        &format!("{KIND_SIGN_IN}:{session_id}"),
        &json!({"title": "Новый вход в canterlot", "body": clip(body, DETAIL_CHARS)}),
    )
    .await
    .map(|_| ())
}

/// Живой сеанс пришёл из новой страны: один пуш на сеанс и страну.
pub async fn on_session_country(
    pool: &PgPool,
    session_id: Uuid,
    country_code: &str,
    body: &str,
) -> Result<(), sqlx::Error> {
    enqueue_auth(
        pool,
        KIND_SESSION_COUNTRY,
        &format!("{KIND_SESSION_COUNTRY}:{session_id}:{country_code}"),
        &json!({"title": "Сеанс из новой страны", "body": clip(body, DETAIL_CHARS)}),
    )
    .await
    .map(|_| ())
}

/// Неудачная попытка входа: не больше одного сводного пуша в час, когда неудач
/// за час набралось не меньше порога. Числа считаются при отправке.
pub async fn on_sign_in_failure(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into owner_pushes(id,conversation_id,kind,dedupe_key,payload,run_at)
         select $1,null,$2,$2||':'||$1::text,'{}'::jsonb,clock_timestamp() from push_settings
         where (select count(*) from auth_failures where at>now()-interval '1 hour') >= $3
           and not exists(select 1 from owner_pushes
                          where kind=$2 and created_at>clock_timestamp()-interval '1 hour')",
    )
    .bind(Uuid::now_v7())
    .bind(KIND_SIGN_IN_FAILURES)
    .bind(SIGN_IN_FAILURE_THRESHOLD)
    .execute(pool)
    .await?;
    Ok(())
}

/// Общая блокировка входа по паролю включилась: один пуш на блокировку.
pub async fn on_sign_in_lock(pool: &PgPool) -> Result<(), sqlx::Error> {
    let id = Uuid::now_v7();
    enqueue_auth(
        pool,
        KIND_SIGN_IN_LOCK,
        &format!("{KIND_SIGN_IN_LOCK}:{id}"),
        &json!({}),
    )
    .await
    .map(|_| ())
}

/// «с 1 адреса», «с 2 адресов», «с 21 адреса».
fn addresses(count: i64) -> String {
    let word = if count % 10 == 1 && count % 100 != 11 {
        "адреса"
    } else {
        "адресов"
    };
    format!("{count} {word}")
}

pub fn sign_in_failures_text(failures: i64, buckets: i64) -> String {
    format!(
        "Неудачные попытки входа: {failures} с {} за час",
        addresses(buckets)
    )
}

async fn compose_auth(
    pool: &PgPool,
    row: &PushRow,
    app_url: &str,
) -> Result<Option<Message>, sqlx::Error> {
    let (title, body, priority) = match row.kind.as_str() {
        KIND_CREDENTIAL_EXPIRY | KIND_CREDENTIAL_AUTH => {
            let Some(machine_id) = row.payload["machine_id"]
                .as_str()
                .and_then(|id| id.parse::<Uuid>().ok())
            else {
                return Ok(None);
            };
            let Some(name): Option<String> =
                sqlx::query_scalar("select name from machines where id=$1")
                    .bind(machine_id)
                    .fetch_optional(pool)
                    .await?
            else {
                return Ok(None);
            };
            let name = clip(&name, TITLE_CHARS);
            if row.kind == KIND_CREDENTIAL_AUTH {
                (
                    "Claude Code просит войти заново".to_owned(),
                    format!("Машина {name}: запустите claude и выполните /login."),
                    PRIORITY_CREDENTIAL_AUTH,
                )
            } else {
                let Some(expires_at) = row.payload["expires_at"]
                    .as_str()
                    .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
                    .map(|at| at.with_timezone(&Utc))
                else {
                    return Ok(None);
                };
                let day = local_day(pool, expires_at).await?;
                (
                    "Вход в Claude Code заканчивается".to_owned(),
                    format!(
                        "Машина {name}: до {day}. Запустите claude на машине и выполните /login."
                    ),
                    PRIORITY_CREDENTIAL_EXPIRY,
                )
            }
        }
        KIND_SIGN_IN | KIND_SESSION_COUNTRY => {
            let (Some(title), Some(body)) =
                (row.payload["title"].as_str(), row.payload["body"].as_str())
            else {
                return Ok(None);
            };
            (title.to_owned(), body.to_owned(), PRIORITY_SIGN_IN)
        }
        KIND_SIGN_IN_FAILURES => {
            let (failures, buckets) = crate::owner_auth::throttle::last_hour(pool).await?;
            if failures == 0 {
                return Ok(None);
            }
            (
                "Попытки входа в canterlot".to_owned(),
                sign_in_failures_text(failures, buckets),
                PRIORITY_SIGN_IN,
            )
        }
        KIND_SIGN_IN_LOCK => (
            "Вход по паролю заблокирован".to_owned(),
            "Слишком много неудачных попыток: вход по паролю закрыт на 10 минут \
             для устройств, где вы ещё не входили"
                .to_owned(),
            PRIORITY_SIGN_IN,
        ),
        KIND_SCHEDULE_FAILED => {
            let name = row.payload["schedule_name"]
                .as_str()
                .unwrap_or("Расписание");
            let reason = row.payload["reason"].as_str().unwrap_or("failed");
            (
                format!("Расписание «{}» не запустилось", clip(name, TITLE_CHARS)),
                format!("Причина: {}.", schedule_failure_text(reason)),
                PRIORITY_FAILURE,
            )
        }
        KIND_CLOUD_ALERT => {
            let Some((title, body)) = cloud_alert_text(&row.payload) else {
                return Ok(None);
            };
            (title, body, PRIORITY_CLOUD_ALERT)
        }
        _ => return Ok(None),
    };
    let click_url = if matches!(
        row.kind.as_str(),
        KIND_CREDENTIAL_EXPIRY | KIND_CREDENTIAL_AUTH | KIND_CLOUD_ALERT
    ) {
        format!("{app_url}/machines")
    } else if row.kind == KIND_SCHEDULE_FAILED {
        format!("{app_url}/settings")
    } else {
        format!("{app_url}{SIGN_IN_PATH}")
    };
    Ok(Some(Message {
        title,
        body,
        priority,
        click_url,
    }))
}

/// Готовое сообщение Gotify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub title: String,
    pub body: String,
    pub priority: u8,
    pub click_url: String,
}

impl Message {
    pub fn to_json(&self) -> Value {
        json!({
            "title": self.title,
            "message": self.body,
            "priority": self.priority,
            "extras": {
                "client::display": {"contentType": "text/plain"},
                "client::notification": {"click": {"url": self.click_url}},
            },
        })
    }
}

/// Итог одного прохода отправки: только числа.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryReport {
    pub sent: usize,
    pub retried: usize,
    pub dropped: usize,
    pub cancelled: usize,
    /// Чат сессии был открыт и владелец уже прочитал событие: пуш не отправлен.
    pub suppressed: usize,
    /// Строку отложили из-за открытого чата; она вернётся через минуту.
    pub deferred_visible: usize,
    /// Отложены общим пределом в минуту.
    pub deferred: usize,
}

impl DeliveryReport {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

enum Failure {
    /// Сеть, срок запроса, 5xx, 408, 429: стоит повторить.
    Retry(Option<u16>),
    /// Остальные ответы 4xx: повтор ничего не изменит.
    Fatal(u16),
}

#[derive(sqlx::FromRow)]
struct PushRow {
    id: Uuid,
    conversation_id: Option<Uuid>,
    kind: String,
    payload: Value,
    attempts: i32,
    visible_defers: i32,
    created_at: DateTime<Utc>,
    /// Версия строки, которую видел отправитель: условие сброса устаревшего.
    updated_at: DateTime<Utc>,
    stale: bool,
    /// Попытка отмечена в строке, а итога нет дольше грейса: хаб погиб посреди отправки.
    stranded: bool,
    /// Отметка отправки жива: первая попытка могла дойти, повторять POST нельзя.
    sending: bool,
}

/// Отправитель пушей. Ключ живёт только здесь и уходит лишь в заголовке `X-Gotify-Key`.
#[derive(Clone)]
pub struct Sender {
    client: reqwest::Client,
    endpoint: String,
    token: GotifyToken,
    app_url: String,
    max_per_minute: usize,
    backoff: Vec<Duration>,
    window: Arc<Mutex<VecDeque<Instant>>>,
    presence: Presence,
}

impl std::fmt::Debug for Sender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sender")
            .field("endpoint", &self.endpoint)
            .field("token", &self.token)
            .finish_non_exhaustive()
    }
}

impl Sender {
    pub fn new(config: &GotifyConfig) -> Result<Self, &'static str> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "notify_client_error")?;
        Ok(Self {
            client,
            endpoint: format!("{}/message", config.url),
            token: config.token.clone(),
            app_url: config.app_url().to_owned(),
            max_per_minute: usize::try_from(config.max_per_minute).unwrap_or(usize::MAX),
            // Три попытки примерно за пять минут.
            backoff: vec![Duration::from_secs(60), Duration::from_secs(180)],
            window: Arc::default(),
            presence: Presence::default(),
        })
    }

    /// Паузы между попытками; попыток на одну больше, чем пауз.
    pub fn with_backoff(mut self, backoff: Vec<Duration>) -> Self {
        self.backoff = backoff;
        self
    }

    /// Карта присутствия, общая с HTTP-слоем: открытый чат откладывает пуши.
    pub fn with_presence(mut self, presence: Presence) -> Self {
        self.presence = presence;
        self
    }

    fn max_attempts(&self) -> i32 {
        i32::try_from(self.backoff.len() + 1).unwrap_or(i32::MAX)
    }

    /// Скользящее окно минуты: место есть — оно занимается сразу.
    fn take_slot(&self) -> bool {
        let mut window = self.window.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        while window
            .front()
            .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(60))
        {
            window.pop_front();
        }
        if window.len() >= self.max_per_minute {
            return false;
        }
        window.push_back(now);
        true
    }

    async fn post(&self, message: &Message) -> Result<u16, Failure> {
        let mut key = reqwest::header::HeaderValue::from_str(self.token.expose())
            .map_err(|_| Failure::Fatal(0))?;
        key.set_sensitive(true);
        let response = self
            .client
            .post(&self.endpoint)
            .header("X-Gotify-Key", key)
            .json(&message.to_json())
            .send()
            .await
            .map_err(|_| Failure::Retry(None))?;
        let status = response.status().as_u16();
        drop(response);
        match status {
            200..=299 => Ok(status),
            408 | 429 | 500..=599 => Err(Failure::Retry(Some(status))),
            _ => Err(Failure::Fatal(status)),
        }
    }

    /// Отправка мимо очереди: ссылка восстановления не должна лежать в базе открытым текстом.
    pub async fn send_now(&self, message: &Message) -> Result<u16, &'static str> {
        if !self.take_slot() {
            return Err("notify_rate_limited");
        }
        self.post(message).await.map_err(|failure| match failure {
            Failure::Retry(_) => "notify_unavailable",
            Failure::Fatal(_) => "notify_rejected",
        })
    }

    /// Один проход: забирает созревшие строки и отправляет их по одной.
    /// И готовность, и протухание считает база: часы хаба и базы могут расходиться.
    pub async fn deliver_due(&self, pool: &PgPool) -> Result<DeliveryReport, sqlx::Error> {
        self.deliver_due_at(pool, Instant::now()).await
    }

    /// Тот же проход с явными часами: срок присутствия проверяется без ожидания.
    pub async fn deliver_due_at(
        &self,
        pool: &PgPool,
        now: Instant,
    ) -> Result<DeliveryReport, sqlx::Error> {
        let rows: Vec<PushRow> = sqlx::query_as(
            "select id,conversation_id,kind,payload,attempts,visible_defers,created_at,updated_at,
                    clock_timestamp()-created_at > $1 * interval '1 second' as stale,
                    coalesce(last_status='sending'
                             and clock_timestamp()-updated_at > $3 * interval '1 second',
                             false) as stranded,
                    coalesce(last_status='sending', false) as sending
             from owner_pushes
             where state='pending' and run_at<=clock_timestamp()
             order by run_at,id limit $2",
        )
        .bind(STALE.num_seconds() as f64)
        .bind(BATCH)
        .bind(SENDING_GRACE_SECS)
        .fetch_all(pool)
        .await?;
        let mut report = DeliveryReport::default();
        for row in rows {
            // Пуш «отчёт готов» не повторяется после гибели хаба посреди отправки: Gotify мог
            // принять запрос. Попытка отмечена в строке до POST (`begin_attempt`).
            if row.sending && !row.stranded && row.kind == KIND_ROUTINE_REPORT {
                continue;
            }
            if row.stranded && row.kind == KIND_ROUTINE_REPORT {
                finish(pool, row.id, "dropped", Some("ambiguous_restart")).await?;
                warn_dropped(&row, None, "ambiguous_restart");
                report.dropped += 1;
                continue;
            }
            if row.stale {
                // Условно по версии: слияние свежего исхода после чтения строки
                // меняет `updated_at`, и такая строка уже не устарела.
                let dropped = sqlx::query(
                    "update owner_pushes set state='dropped',updated_at=clock_timestamp()
                     where id=$1 and state='pending' and updated_at=$2",
                )
                .bind(row.id)
                .bind(row.updated_at)
                .execute(pool)
                .await?
                .rows_affected();
                if dropped > 0 {
                    warn_dropped(&row, None, "stale");
                    report.dropped += 1;
                }
                continue;
            }
            // Открытый чат сам по себе не повод бросить пуш: телефон мог уснуть, так и не
            // прислав «скрыто». Сначала проверяем, что владелец действительно прочитал
            // событие; иначе откладываем строку и вернёмся к ней через минуту.
            if let Some(conversation_id) = row.conversation_id
                && self.presence.is_visible(conversation_id, now)
            {
                if owner_seen(pool, &row).await? {
                    finish(pool, row.id, "suppressed", Some("suppressed_seen")).await?;
                    report.suppressed += 1;
                    continue;
                }
                if row.visible_defers < VISIBLE_RETRIES {
                    sqlx::query(
                        "update owner_pushes set visible_defers=visible_defers+1,
                           run_at=clock_timestamp()+$2*interval '1 second',
                           last_status='deferred_visible',updated_at=clock_timestamp()
                         where id=$1 and state='pending'",
                    )
                    .bind(row.id)
                    .bind(VISIBLE_RETRY_SECS)
                    .execute(pool)
                    .await?;
                    report.deferred_visible += 1;
                    continue;
                }
                // Отсрочки кончились: чат, возможно, замер навсегда. Отправляем.
            }
            let Some(message) = compose(pool, &row, &self.app_url).await? else {
                finish(pool, row.id, "cancelled", None).await?;
                report.cancelled += 1;
                continue;
            };
            // Ограничитель пушей о запусках расписания живёт здесь, в момент
            // доставки (6): задержанный первый пуш и поздний второй не
            // оказываются в одном часе.
            let scheduled = row.kind == KIND_SCHEDULE_FAILED;
            if scheduled {
                match schedule_cooldown(pool, &row).await? {
                    Cooldown::Gone => {
                        finish(pool, row.id, "cancelled", Some("schedule_gone")).await?;
                        report.cancelled += 1;
                        continue;
                    }
                    // Активная метка глушит и повтор: она встаёт вместе с попыткой,
                    // и раз она жива, прошлая отправка могла дойти.
                    Cooldown::Active => {
                        finish(pool, row.id, "suppressed", Some("suppressed_cooldown")).await?;
                        report.suppressed += 1;
                        continue;
                    }
                    Cooldown::Clear => {}
                }
            }
            if !self.take_slot() {
                report.deferred += 1;
                break;
            }
            let attempts = row.attempts.saturating_add(1);
            // Попытка и пауза фиксируются до запроса: потерянное подтверждение
            // стоит одного отложенного повтора, а не повторной доставки.
            let pause = self
                .backoff
                .get(usize::try_from(row.attempts).unwrap_or(usize::MAX))
                .copied()
                .unwrap_or_default();
            // Попытка, пауза и метка охлаждения фиксируются одной транзакцией до
            // запроса: потерянное подтверждение или гибель хаба после отправки не
            // оставят строку, которая обойдёт охлаждение при повторе.
            let stamp = begin_attempt(pool, &row, attempts, pause, scheduled).await?;
            let result = self.post(&message).await;
            // Пуш расписания и пуш «отчёт готов»: неоднозначная доставка (обрыв, таймаут,
            // 408, 5xx шлюза после приёма апстримом) не повторяется, метка остаётся.
            // Только определённый отказ возвращает ПРЕЖНЮЮ метку (она есть у пуша расписания).
            if scheduled || row.kind == KIND_ROUTINE_REPORT {
                let ambiguous =
                    matches!(&result, Err(Failure::Retry(None | Some(408 | 500..=599))));
                if ambiguous {
                    let status = match &result {
                        Err(Failure::Retry(status)) => *status,
                        _ => None,
                    };
                    sqlx::query(
                        "update owner_pushes set state='dropped',last_status=$2,
                           updated_at=clock_timestamp()
                         where id=$1 and state='pending'",
                    )
                    .bind(row.id)
                    .bind(match status {
                        Some(code) => format!("ambiguous_{code}"),
                        None => "ambiguous_delivery".to_owned(),
                    })
                    .execute(pool)
                    .await?;
                    warn_dropped(&row, status, "ambiguous_delivery");
                    report.dropped += 1;
                    continue;
                }
                if result.is_err()
                    && scheduled
                    && let Some(stamp) = stamp
                {
                    restore_schedule_push(pool, &row, &stamp).await?;
                }
            }
            match result {
                Ok(status) => {
                    finish(pool, row.id, "sent", Some(&status.to_string())).await?;
                    report.sent += 1;
                }
                Err(Failure::Retry(status)) if attempts < self.max_attempts() => {
                    sqlx::query(
                        "update owner_pushes set last_status=$2,updated_at=clock_timestamp()
                         where id=$1 and state='pending'",
                    )
                    .bind(row.id)
                    .bind(status.map(|code| code.to_string()))
                    .execute(pool)
                    .await?;
                    report.retried += 1;
                }
                Err(failure) => {
                    let status = match failure {
                        Failure::Retry(status) => status,
                        Failure::Fatal(0) => None,
                        Failure::Fatal(status) => Some(status),
                    };
                    sqlx::query(
                        "update owner_pushes set state='dropped',last_status=$2,
                           updated_at=clock_timestamp()
                         where id=$1 and state='pending'",
                    )
                    .bind(row.id)
                    .bind(status.map(|code| code.to_string()))
                    .execute(pool)
                    .await?;
                    warn_dropped(&row, status, "gave_up");
                    report.dropped += 1;
                }
            }
        }
        Ok(report)
    }
}

enum Cooldown {
    /// Расписание удалено: пуш не нужен.
    Gone,
    /// Последний пуш о расписании ушёл меньше часа назад.
    Active,
    Clear,
}

struct Stamp {
    previous: Option<DateTime<Utc>>,
    stamped: DateTime<Utc>,
}

async fn schedule_cooldown(pool: &PgPool, row: &PushRow) -> Result<Cooldown, sqlx::Error> {
    let Some(schedule_id) = row
        .payload
        .get("schedule_id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
    else {
        return Ok(Cooldown::Gone);
    };
    let found: Option<bool> = sqlx::query_scalar(
        "select coalesce(clock_timestamp()-last_failure_push_at < $2 * interval '1 second', false)
           from schedules where id=$1",
    )
    .bind(schedule_id)
    .bind(SCHEDULE_FAILURE_WINDOW.num_seconds() as f64)
    .fetch_optional(pool)
    .await?;
    Ok(match found {
        None => Cooldown::Gone,
        Some(true) => Cooldown::Active,
        Some(false) => Cooldown::Clear,
    })
}

/// Попытка отправки: счётчик, пауза и (для пуша расписания) метка охлаждения
/// встают одной транзакцией. Прежняя метка читается из подзапроса `for update`
/// в `from`, а не из CTE: CTE видела бы строку уже после обновления.
async fn begin_attempt(
    pool: &PgPool,
    row: &PushRow,
    attempts: i32,
    pause: Duration,
    stamp: bool,
) -> Result<Option<Stamp>, sqlx::Error> {
    let mut tx = crate::db::begin(pool).await?;
    // Порядок замков общий с постановкой (`enqueue_schedule_failure_in_tx`):
    // сначала строка расписания, затем строка пуша. Обратный порядок даёт
    // взаимную блокировку (40P01) при одновременном слиянии и отправке.
    let schedule_id = if stamp {
        row.payload
            .get("schedule_id")
            .and_then(Value::as_str)
            .and_then(|id| Uuid::parse_str(id).ok())
    } else {
        None
    };
    let previous: Option<Option<DateTime<Utc>>> = match schedule_id {
        Some(schedule_id) => {
            sqlx::query_scalar("select last_failure_push_at from schedules where id=$1 for update")
                .bind(schedule_id)
                .fetch_optional(&mut *tx)
                .await?
        }
        None => None,
    };
    sqlx::query(
        "update owner_pushes set attempts=$2,
           run_at=clock_timestamp()+$3*interval '1 second',
           last_status=case when $4 then 'sending' else last_status end,
           updated_at=clock_timestamp()
         where id=$1 and state='pending'",
    )
    .bind(row.id)
    .bind(attempts)
    .bind(pause.as_secs_f64())
    .bind(row.kind == KIND_ROUTINE_REPORT)
    .execute(&mut *tx)
    .await?;
    let stamped = match (schedule_id, previous) {
        (Some(schedule_id), Some(previous)) => {
            let stamped: DateTime<Utc> = sqlx::query_scalar(
                "update schedules set last_failure_push_at=clock_timestamp()
                  where id=$1 returning last_failure_push_at",
            )
            .bind(schedule_id)
            .fetch_one(&mut *tx)
            .await?;
            Some(Stamp { previous, stamped })
        }
        _ => None,
    };
    tx.commit().await?;
    Ok(stamped)
}

async fn restore_schedule_push(
    pool: &PgPool,
    row: &PushRow,
    stamp: &Stamp,
) -> Result<(), sqlx::Error> {
    let Some(schedule_id) = row
        .payload
        .get("schedule_id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
    else {
        return Ok(());
    };
    sqlx::query(
        "update schedules set last_failure_push_at=$2
          where id=$1 and last_failure_push_at=$3",
    )
    .bind(schedule_id)
    .bind(stamp.previous)
    .bind(stamp.stamped)
    .execute(pool)
    .await?;
    Ok(())
}

/// Владелец уже прочитал событие, из-за которого поставлен пуш.
///
/// Ссылка — место в ленте (`seq`) в payload. У сбоя такого места нет: состояние
/// сессии не появляется в ленте, поэтому пуш о сбое не подавляется, а доходит
/// после отсрочек.
async fn owner_seen(pool: &PgPool, row: &PushRow) -> Result<bool, sqlx::Error> {
    let Some(seq) = row.payload.get("seq").and_then(Value::as_i64) else {
        return Ok(false);
    };
    let Some(conversation_id) = row.conversation_id else {
        return Ok(false);
    };
    let seen: Option<bool> =
        sqlx::query_scalar("select last_read_seq >= $2 from conversations where id=$1")
            .bind(conversation_id)
            .bind(seq)
            .fetch_optional(pool)
            .await?;
    Ok(seen.unwrap_or(false))
}

async fn finish(
    pool: &PgPool,
    id: Uuid,
    state: &str,
    status: Option<&str>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update owner_pushes set state=$2,last_status=coalesce($3,last_status),
           updated_at=clock_timestamp()
         where id=$1 and state='pending'",
    )
    .bind(id)
    .bind(state)
    .bind(status)
    .execute(pool)
    .await?;
    Ok(())
}

/// Одна строка на брошенный пуш: вид, сессия и код ответа, без текста и ключа.
fn warn_dropped(row: &PushRow, status: Option<u16>, reason: &'static str) {
    tracing::warn!(
        kind = "owner_push_dropped",
        push_kind = %row.kind,
        conversation_id = ?row.conversation_id,
        status = status.map(i64::from),
        reason,
        "owner push dropped"
    );
}

/// Текст пуша по текущему состоянию базы. `None` — пуш больше не нужен: сессия в корзине
/// или запрос уже решён.
async fn compose(
    pool: &PgPool,
    row: &PushRow,
    app_url: &str,
) -> Result<Option<Message>, sqlx::Error> {
    let Some(conversation_id) = row.conversation_id else {
        return compose_auth(pool, row, app_url).await;
    };
    let conversation: Option<(String, bool)> = sqlx::query_as(
        "select title,trashed_at is not null from conversations where id=$1 and side_of is null",
    )
    .bind(conversation_id)
    .fetch_optional(pool)
    .await?;
    let Some((title, trashed)) = conversation else {
        return Ok(None);
    };
    if trashed {
        return Ok(None);
    }
    if row.kind == KIND_ROUTINE_REPORT {
        // Карточку успели закрыть или заменить: пуш потерял смысл.
        let Some(report_id) = row.payload["report_id"]
            .as_str()
            .and_then(|id| id.parse::<Uuid>().ok())
        else {
            return Ok(None);
        };
        let open: Option<bool> = sqlx::query_scalar(
            "select state='ready' from routine_reports
              where id=$1 and conversation_id=$2 and dismissed_at is null",
        )
        .bind(report_id)
        .bind(conversation_id)
        .fetch_optional(pool)
        .await?;
        if open != Some(true) {
            return Ok(None);
        }
        let name = row.payload["schedule_name"].as_str().unwrap_or_default();
        return Ok(Some(Message {
            title: format!("{} готов", clip(name, TITLE_CHARS)),
            body: clean_title(&title),
            priority: PRIORITY_TURN_FINISHED,
            // Ссылка ведёт в родителя; карточку клиент находит по идентификатору в запросе.
            click_url: format!("{app_url}/c/{conversation_id}?routine_report={report_id}"),
        }));
    }
    let (body, priority) = match row.kind.as_str() {
        KIND_WAITING if row.payload.get("secret_request_id").is_some() => {
            let Some(secret_id) = row.payload["secret_request_id"]
                .as_str()
                .and_then(|id| id.parse::<Uuid>().ok())
            else {
                return Ok(None);
            };
            let secret: Option<(String, String)> = sqlx::query_as(
                "select name,state from secret_requests where id=$1 and conversation_id=$2",
            )
            .bind(secret_id)
            .bind(conversation_id)
            .fetch_optional(pool)
            .await?;
            match secret {
                Some((name, state)) if matches!(state.as_str(), "open" | "lost") => (
                    format!("Агент просит секрет «{}»", clip(&name, TITLE_CHARS)),
                    PRIORITY_WAITING,
                ),
                _ => return Ok(None),
            }
        }
        KIND_WAITING => {
            let Some(request_id) = row.payload["request_id"]
                .as_str()
                .and_then(|id| id.parse::<Uuid>().ok())
            else {
                return Ok(None);
            };
            let request: Option<(String, String, Value)> = sqlx::query_as(
                "select kind,state,request_payload from requests where id=$1 and conversation_id=$2",
            )
            .bind(request_id)
            .bind(conversation_id)
            .fetch_optional(pool)
            .await?;
            match request {
                Some((kind, state, payload)) if state == "open" => {
                    (waiting_text(&kind, &payload), PRIORITY_WAITING)
                }
                _ => return Ok(None),
            }
        }
        KIND_TURN_FINISHED => {
            // Ответ берём только из последнего хода: текст прошлого хода не должен
            // выдаваться за итог хода, кончившегося без ответа.
            let started: Option<DateTime<Utc>> = sqlx::query_scalar(
                "select max(started_at) from runs where conversation_id=$1 and finished_at <= $2",
            )
            .bind(conversation_id)
            .bind(row.created_at)
            .fetch_one(pool)
            .await?;
            let answer: Option<String> = match started {
                Some(started) => {
                    let query = format!(
                        "select left(content,4000) from blocks
                         where conversation_id=$1 and kind='text' and parent_block_id is null
                           and btrim(content) <> '' and created_at >= $2 and created_at <= $3
                           and {}
                         order by created_seq desc nulls last,created_at desc limit 1",
                        crate::agents::main_agent_filter("agent")
                    );
                    sqlx::query_scalar(sqlx::AssertSqlSafe(query))
                        .bind(conversation_id)
                        .bind(started)
                        .bind(row.created_at)
                        .fetch_optional(pool)
                        .await?
                }
                None => None,
            };
            let text = if row.payload["final"].as_bool() == Some(true) {
                all_done_text(answer.as_deref())
            } else {
                turn_text(answer.as_deref())
            };
            (text, PRIORITY_TURN_FINISHED)
        }
        KIND_FAILURE => {
            let at = failure_time(&row.payload["evidence"]);
            let local = match at {
                Some(at) => Some(local_time(pool, at).await?),
                None => None,
            };
            (
                failure_text(
                    row.payload["state"].as_str().unwrap_or_default(),
                    &row.payload["evidence"],
                    local.as_deref(),
                ),
                PRIORITY_FAILURE,
            )
        }
        _ => return Ok(None),
    };
    Ok(Some(Message {
        title: clean_title(&title),
        body,
        priority,
        click_url: format!("{app_url}/c/{}", conversation_id),
    }))
}

/// Время, которое назовёт текст сбоя: конец паузы по лимиту или последнее событие застрявшей.
fn failure_time(evidence: &Value) -> Option<DateTime<Utc>> {
    let field = match evidence["kind"].as_str()? {
        "quota_paused" => "until",
        "stalled" => "last_event_at",
        _ => return None,
    };
    evidence[field]
        .as_str()
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .map(|at| at.with_timezone(&Utc))
}

async fn local_time(pool: &PgPool, at: DateTime<Utc>) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "select to_char($1 at time zone coalesce((select timezone from push_settings),'UTC'),
                        'DD.MM HH24:MI')",
    )
    .bind(at)
    .fetch_one(pool)
    .await
}

/// Дата в часовом поясе владельца: «12.12», как в подсказках PWA.
async fn local_day(pool: &PgPool, at: DateTime<Utc>) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "select to_char($1 at time zone coalesce((select timezone from push_settings),'UTC'),
                        'DD.MM')",
    )
    .bind(at)
    .fetch_one(pool)
    .await
}

/// Строка без управляющих символов и лишних пробелов, не длиннее `limit` символов.
pub fn clip(text: &str, limit: usize) -> String {
    let plain: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let mut cut: String = flat.chars().take(limit.saturating_sub(1)).collect();
    cut.truncate(cut.trim_end().len());
    cut.push('…');
    cut
}

pub fn clean_title(title: &str) -> String {
    let title = clip(title, TITLE_CHARS);
    if title.is_empty() {
        "Canterlot".into()
    } else {
        title
    }
}

fn text_field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| value.get(*key).and_then(Value::as_str))
        .map(str::trim)
        .find(|text| !text.is_empty())
}

/// Сколько вопросов в запросе: тот же набор форм, что понимает `extract_request_summary`.
fn question_count(payload: &Value) -> usize {
    payload
        .get("questions")
        .or_else(|| payload.get("ask_question").and_then(|a| a.get("questions")))
        .or_else(|| payload.get("payload").and_then(|p| p.get("questions")))
        .and_then(Value::as_array)
        .map_or(1, Vec::len)
}

/// Текст пуша о запросе. Ответов в нём нет: запрос ещё открыт, а секретный ответ
/// хаб и так нигде не показывает.
pub fn waiting_text(kind: &str, payload: &Value) -> String {
    match kind {
        "question" => {
            let first = crate::activity::extract_request_summary(
                crate::requests::RequestKind::Question,
                payload,
            );
            match first
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty())
            {
                Some(text) => {
                    let more = question_count(payload).saturating_sub(1);
                    let suffix = if more > 0 {
                        format!(" (+{more})")
                    } else {
                        String::new()
                    };
                    format!("Ждёт ответа: {}{suffix}", clip(&text, DETAIL_CHARS))
                }
                None => "Ждёт ответа".into(),
            }
        }
        "permission" => {
            let tool = text_field(payload, &["tool_name", "tool"]);
            if tool == Some("ExitPlanMode") {
                return "Ждёт одобрения плана".into();
            }
            let input = payload.get("input").unwrap_or(&Value::Null);
            let detail = text_field(payload, &["command", "description"])
                .or_else(|| text_field(input, &["command", "description", "file_path"]));
            match (tool, detail) {
                (Some(tool), Some(detail)) => {
                    format!("Ждёт разрешения: {tool}: {}", clip(detail, DETAIL_CHARS))
                }
                (Some(tool), None) => format!("Ждёт разрешения: {}", clip(tool, DETAIL_CHARS)),
                (None, Some(detail)) => {
                    format!("Ждёт разрешения: {}", clip(detail, DETAIL_CHARS))
                }
                (None, None) => "Ждёт разрешения".into(),
            }
        }
        "spawn_approval" => match text_field(payload, &["harness"]) {
            Some(harness) => format!("Ждёт одобрения запуска: {}", clip(harness, 40)),
            None => "Ждёт одобрения запуска".into(),
        },
        "settings_change" => match text_field(payload, &["setting"]) {
            Some(setting) => format!("Ждёт решения по настройке: {}", clip(setting, 40)),
            None => "Ждёт решения по настройке".into(),
        },
        _ => "Ждёт ответа".into(),
    }
}

pub fn turn_text(answer: Option<&str>) -> String {
    match answer.map(|text| clip(text, ANSWER_CHARS)) {
        Some(text) if !text.is_empty() => format!("Ход завершён\n{text}"),
        _ => "Ход завершён".into(),
    }
}

/// Итог запроса владельца: последний ход может быть и служебным, но работа кончилась.
pub fn all_done_text(answer: Option<&str>) -> String {
    match answer.map(|text| clip(text, ANSWER_CHARS)) {
        Some(text) if !text.is_empty() => format!("Всё готово\n{text}"),
        _ => "Всё готово".into(),
    }
}

pub fn failure_text(state: &str, evidence: &Value, local_time: Option<&str>) -> String {
    match state {
        "hung" if evidence["second"].as_bool() == Some(true) => {
            "Ход снова завис в генерации и остаётся зависшим".into()
        }
        "hung" => "Ход завис в генерации".into(),
        "stalled" => match local_time {
            Some(at) => format!("Застряла: нет событий с {at}"),
            None => "Застряла".into(),
        },
        "quota_paused" => match local_time {
            Some(at) => format!("Пауза по лимиту до {at}"),
            None => "Пауза по лимиту".into(),
        },
        OWNER_LOST => "Сессия упала: воркер потерян".into(),
        _ => match text_field(evidence, &["reason"]) {
            Some(reason) => format!("Сессия упала: {}", clip(reason, DETAIL_CHARS)),
            None => "Сессия упала".into(),
        },
    }
}

/// Фоновая служба: одна на базу, опрос раз в секунду, уборка готовых строк раз в час.
pub async fn run(pool: PgPool, sender: Sender, shutdown: watch::Receiver<bool>) {
    crate::singleton::supervise(&pool, "owner_push", shutdown, || {
        let pool = pool.clone();
        let sender = sender.clone();
        async move {
            let mut cleaned_at: Option<Instant> = None;
            loop {
                if cleaned_at.is_none_or(|at| at.elapsed() >= Duration::from_secs(3600)) {
                    cleaned_at = Some(Instant::now());
                    if let Err(error) = sqlx::query(
                        "delete from owner_pushes where state<>'pending'
                           and updated_at<clock_timestamp()-$1*interval '1 second'",
                    )
                    .bind(RETENTION.num_seconds() as f64)
                    .execute(&pool)
                    .await
                    {
                        log_error(&error);
                    }
                }
                match sender.deliver_due(&pool).await {
                    Ok(report) if !report.is_empty() => tracing::info!(
                        kind = "owner_push_delivery",
                        sent = report.sent,
                        retried = report.retried,
                        dropped = report.dropped,
                        cancelled = report.cancelled,
                        suppressed = report.suppressed,
                        deferred_visible = report.deferred_visible,
                        deferred = report.deferred,
                        "owner pushes processed"
                    ),
                    Ok(_) => {}
                    Err(error) => log_error(&error),
                }
                tokio::time::sleep(POLL).await;
            }
        }
    })
    .await;
}

fn log_error(error: &sqlx::Error) {
    tracing::warn!(
        kind = "owner_push_db_error",
        error = %crate::dberr::describe(error),
        "owner push delivery failed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Постановка (слияние) и попытка отправки берут строки в одном порядке:
    /// расписание, затем пуш. Расписание держит «постановку», попытка ждёт его,
    /// затем «постановка» идёт к строке пуша: при обратном порядке попытки это
    /// взаимная блокировка (40P01), при общем порядке обе завершаются.
    #[tokio::test]
    async fn enqueue_and_send_attempt_lock_rows_in_one_order() {
        let pool = crate::testkit::db().await;
        let seed = crate::testkit::seed(&pool).await;
        configure(
            &pool,
            Some(&PushSettings {
                waiting: true,
                turn_finished: true,
                failures: true,
                timezone: "UTC".into(),
            }),
        )
        .await
        .unwrap();
        let schedule = Uuid::now_v7();
        sqlx::query(
            "insert into schedules(id,owner_id,name,cron,timezone,target,prompt,next_fire_at)
             values($1,$2,'s','0 9 * * *','UTC','{\"kind\":\"existing_session\"}','p',now())",
        )
        .bind(schedule)
        .bind(seed.owner_id)
        .execute(&pool)
        .await
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        assert!(
            enqueue_schedule_failure_in_tx(&mut conn, schedule, "s", "first")
                .await
                .unwrap()
        );
        drop(conn);
        let (id, payload, created_at, updated_at): (Uuid, Value, DateTime<Utc>, DateTime<Utc>) =
            sqlx::query_as(
                "select id,payload,created_at,updated_at from owner_pushes where kind=$1",
            )
            .bind(KIND_SCHEDULE_FAILED)
            .fetch_one(&pool)
            .await
            .unwrap();
        let row = PushRow {
            id,
            conversation_id: None,
            kind: KIND_SCHEDULE_FAILED.to_owned(),
            payload,
            attempts: 0,
            visible_defers: 0,
            created_at,
            updated_at,
            stale: false,
            stranded: false,
            sending: false,
        };
        // Расписание держит «постановка» (она запрашивает его первым).
        let mut enqueue = crate::db::begin(&pool).await.unwrap();
        sqlx::query("select id from schedules where id=$1 for update")
            .bind(schedule)
            .execute(&mut *enqueue)
            .await
            .unwrap();
        let attempt = {
            let pool = pool.clone();
            tokio::spawn(async move {
                begin_attempt(&pool, &row, 1, Duration::from_secs(60), true).await
            })
        };
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let waiting: i64 = sqlx::query_scalar(
                    "select count(*) from pg_stat_activity
                      where wait_event_type='Lock' and datname=current_database()",
                )
                .fetch_one(&pool)
                .await
                .unwrap();
                if waiting > 0 {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the attempt waits on the schedule row");
        let merged = enqueue_schedule_failure_in_tx(&mut enqueue, schedule, "s", "second").await;
        assert!(
            merged.is_ok(),
            "enqueue must not deadlock: {:?}",
            merged.as_ref().err().map(crate::dberr::describe)
        );
        enqueue.commit().await.unwrap();
        let stamp = attempt.await.unwrap();
        assert!(
            stamp.as_ref().is_ok_and(Option::is_some),
            "attempt must not deadlock: {:?}",
            stamp.as_ref().err().map(crate::dberr::describe)
        );
    }

    #[test]
    fn clip_flattens_and_marks_the_cut() {
        assert_eq!(clip("  a\n\tb  ", 10), "a b");
        assert_eq!(clip("абвгдеёжз", 5), "абвг…");
    }

    #[test]
    fn title_falls_back_and_drops_control_characters() {
        assert_eq!(clean_title(""), "Canterlot");
        assert_eq!(clean_title("a\u{7}\nb"), "a b");
        assert!(clean_title(&"x".repeat(200)).chars().count() <= TITLE_CHARS);
    }

    #[test]
    fn waiting_texts_name_the_request() {
        let question = json!({"questions":[{"id":"q1","text":"Деплоить?","is_secret":true},{"id":"q2","text":"Ещё"}]});
        assert_eq!(
            waiting_text("question", &question),
            "Ждёт ответа: Деплоить? (+1)"
        );
        assert_eq!(
            waiting_text(
                "question",
                &json!({"ask_question":{"questions":["Деплоить?"]}})
            ),
            "Ждёт ответа: Деплоить?"
        );
        assert_eq!(
            waiting_text("question", &json!({"questions":[{"prompt":"Деплоить?"}]})),
            "Ждёт ответа: Деплоить?"
        );
        assert_eq!(
            waiting_text(
                "permission",
                &json!({"tool_name":"ExitPlanMode","input":{"plan":"x"}})
            ),
            "Ждёт одобрения плана"
        );
        assert_eq!(
            waiting_text(
                "permission",
                &json!({"tool_name":"Bash","input":{"command":"ls -la"}})
            ),
            "Ждёт разрешения: Bash: ls -la"
        );
        assert_eq!(
            waiting_text(
                "spawn_approval",
                &json!({"harness":"codex","brief":"секрет"})
            ),
            "Ждёт одобрения запуска: codex"
        );
        assert_eq!(
            waiting_text("settings_change", &json!({"setting":"routing"})),
            "Ждёт решения по настройке: routing"
        );
    }

    #[test]
    fn turn_texts_name_the_end_of_the_request() {
        assert_eq!(turn_text(Some("готово")), "Ход завершён\nготово");
        assert_eq!(turn_text(None), "Ход завершён");
        assert_eq!(all_done_text(Some("готово")), "Всё готово\nготово");
        assert_eq!(all_done_text(None), "Всё готово");
    }

    #[test]
    fn failure_texts() {
        assert_eq!(
            failure_text(
                "failed",
                &json!({"kind":"failed","reason":"process_exited"}),
                None
            ),
            "Сессия упала: process_exited"
        );
        assert_eq!(
            failure_text(OWNER_LOST, &json!({}), None),
            "Сессия упала: воркер потерян"
        );
        assert_eq!(
            failure_text("quota_paused", &json!({}), Some("10.11 14:00")),
            "Пауза по лимиту до 10.11 14:00"
        );
        assert_eq!(failure_text("stalled", &json!({}), None), "Застряла");
        assert_eq!(
            failure_text("hung", &json!({"kind":"hung","second":false}), None),
            "Ход завис в генерации"
        );
        assert_eq!(
            failure_text("hung", &json!({"kind":"hung","second":true}), None),
            "Ход снова завис в генерации и остаётся зависшим"
        );
    }

    #[test]
    fn every_emitted_cloud_alert_code_renders_a_title() {
        let codes = [
            "auto_started",
            "budget_80",
            "budget_100",
            "stray_server_deleted",
            "stray_server_stuck",
            "stray_disk_deleted",
            "stray_disk_stuck",
            "delete_failing",
            "disk_delete_failing",
            "preempted",
            "vm_broken",
            "prepare_failed",
            "replacement_started",
            "replacement_skipped",
            "window_overrun",
            "outage_extra_cost",
            "window_gone",
            "hard_cap",
            "cloud_window_hard_cap",
            "ownership_mismatch",
            "late_create",
            "late_disk",
            "reprice_growth",
        ];
        for code in codes {
            let payload = json!({
                "code": code,
                "name": "canterlot-runner-x",
                "window_id": "018f0000-0000-7000-8000-000000000001",
                "percent": 80,
                "spent_kopecks": 1000,
                "budget_kopecks": 2000,
                "reservation_kopecks": 3000,
                "approved_kopecks": 2000,
                "extra_kopecks": 100,
                "instance_id": "fhmfakeinstance0001",
                "reason": "budget",
                "deadline": "2026-11-24T15:40:00Z",
            });
            let (title, body) =
                cloud_alert_text(&payload).unwrap_or_else(|| panic!("{code} is not rendered"));
            assert!(!title.trim().is_empty(), "{code} has an empty title");
            assert!(!body.trim().is_empty(), "{code} has an empty body");
        }
        // Коды прежнего провайдера больше не выпускаются и не должны рендериться.
        for dead in [
            "no_paid",
            "delete_needs_confirmation",
            "dnat_refused",
            "dnat_delete_failing",
            "stray_dnat_deleted",
            "stray_dnat_stuck",
            "ipv4_refused",
            "stray_ip_deleted",
            "stray_ip_stuck",
            "ip_delete_failing",
            "ipv4_unattributed",
        ] {
            assert!(
                cloud_alert_text(&json!({"code": dead, "name": "x"})).is_none(),
                "{dead} is dead"
            );
        }
    }
}
