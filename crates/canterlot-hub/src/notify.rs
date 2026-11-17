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

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
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
    let sql = format!(
        "insert into owner_pushes(id,conversation_id,kind,dedupe_key,payload,run_at)
         select $1,c.id,$3,$4,$5,clock_timestamp()
         from push_settings s join conversations c on c.id=$2
         where c.trashed_at is null
           and c.side_of is null
           and (not $6 or c.parent_conversation_id is null)
           and ($3 <> 'turn_finished' or c.snoozed_until is null or c.snoozed_until <= clock_timestamp())
           and case $3 when 'waiting' then s.waiting
                       when 'turn_finished' then s.turn_finished
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
            // Сбой пушится один раз на эпизод: дребезг состояния внутри того же
            // исполнения и причины не повторяет пуш.
            enqueue_failure_in_tx(conn, conversation_id, state, fingerprint, evidence).await?;
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

/// Пуш о сбое привязан к эпизоду: исполнение, состояние и причина. Внутри эпизода
/// пуш ставится один раз; если владелец прочитал сессию после первой постановки,
/// новая постановка не делается. Новое исполнение, состояние или причина — новый эпизод.
async fn enqueue_failure_in_tx(
    conn: &mut PgConnection,
    conversation_id: Uuid,
    state: Settled,
    fingerprint: &str,
    evidence: &Evidence,
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
    enqueue_in_tx(
        conn,
        conversation_id,
        KIND_FAILURE,
        &key,
        &json!({"state": state_name(state), "evidence": evidence}),
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
        _ => return Ok(None),
    };
    Ok(Some(Message {
        title,
        body,
        priority,
        click_url: if matches!(
            row.kind.as_str(),
            KIND_CREDENTIAL_EXPIRY | KIND_CREDENTIAL_AUTH
        ) {
            format!("{app_url}/machines")
        } else {
            format!("{app_url}{SIGN_IN_PATH}")
        },
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
    stale: bool,
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
            "select id,conversation_id,kind,payload,attempts,visible_defers,created_at,
                    clock_timestamp()-created_at > $1 * interval '1 second' as stale
             from owner_pushes
             where state='pending' and run_at<=clock_timestamp()
             order by run_at,id limit $2",
        )
        .bind(STALE.num_seconds() as f64)
        .bind(BATCH)
        .fetch_all(pool)
        .await?;
        let mut report = DeliveryReport::default();
        for row in rows {
            if row.stale {
                finish(pool, row.id, "dropped", None).await?;
                warn_dropped(&row, None, "stale");
                report.dropped += 1;
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
            sqlx::query(
                "update owner_pushes set attempts=$2,
                   run_at=clock_timestamp()+$3*interval '1 second',
                   updated_at=clock_timestamp()
                 where id=$1 and state='pending'",
            )
            .bind(row.id)
            .bind(attempts)
            .bind(pause.as_secs_f64())
            .execute(pool)
            .await?;
            match self.post(&message).await {
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
    }
}
