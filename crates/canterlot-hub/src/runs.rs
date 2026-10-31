use crate::blocks::ExecutionRef;
use canterlot_proto::worker::v1::RunFinished;
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

const EXACT_WINDOW: chrono::Duration = chrono::Duration::seconds(60);

async fn provider_of_input(
    conn: &mut PgConnection,
    input_command_id: Uuid,
    run: &ExecutionRef,
) -> Result<Option<Option<&'static str>>, sqlx::Error> {
    let harness: Option<String> = sqlx::query_scalar("select c.harness from conversations c join commands cmd on cmd.conversation_id=c.id where cmd.id=$1 and cmd.kind='deliver_input' and cmd.execution_id=$2 and c.id=$3 and c.machine_id=$4")
        .bind(input_command_id).bind(run.execution_id).bind(run.conversation_id).bind(run.machine_id).fetch_optional(&mut *conn).await?;
    Ok(harness.map(|harness| canterlot_proto::harness::provider_of(&harness)))
}

pub async fn start_in_tx(
    conn: &mut PgConnection,
    input_command_id: Uuid,
    run: &ExecutionRef,
    at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let Some(provider) = provider_of_input(conn, input_command_id, run).await? else {
        return Ok(());
    };
    let inserted = sqlx::query("insert into runs(input_command_id,execution_id,conversation_id,provider,started_at,weekly_before_permille,exact) values($1,$2,$3,$4,$5,(select weekly_used_permille from provider_limits where provider=$4),$6) on conflict do nothing")
        .bind(input_command_id).bind(run.execution_id).bind(run.conversation_id).bind(provider).bind(at).bind(now - at <= EXACT_WINDOW).execute(&mut *conn).await?.rows_affected();
    if inserted > 0
        && let Some(provider) = provider
    {
        refresh_overlap(conn, provider, at).await?;
    }
    Ok(())
}

pub async fn finish_in_tx(
    conn: &mut PgConnection,
    fact: &RunFinished,
    run: &ExecutionRef,
    at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    let command = Uuid::parse_str(&fact.input_command_id)
        .map_err(|_| sqlx::Error::InvalidArgument("invalid run command".into()))?;
    let Some(provider) = provider_of_input(conn, command, run).await? else {
        return Ok(());
    };
    let duration = number(fact.duration_ms)?;
    let restored = chrono::Duration::try_milliseconds(duration)
        .and_then(|d| at.checked_sub_signed(d))
        .ok_or_else(|| sqlx::Error::InvalidArgument("invalid run duration".into()))?;
    // Ход без сохранённого начала восстанавливается по длительности и исключается из оценки.
    sqlx::query("insert into runs(input_command_id,execution_id,conversation_id,provider,started_at,exact) values($1,$2,$3,$4,$5,false) on conflict do nothing")
        .bind(command).bind(run.execution_id).bind(run.conversation_id).bind(provider).bind(restored).execute(&mut *conn).await?;
    let started: DateTime<Utc> =
        sqlx::query_scalar("select started_at from runs where input_command_id=$1")
            .bind(command)
            .fetch_one(&mut *conn)
            .await?;
    if let Some(provider) = provider {
        lock_provider_runs(conn, provider, started).await?;
    }
    let changed=sqlx::query("update runs set finished_at=started_at+($2::bigint * interval '1 millisecond'),duration_ms=$2,context_tokens=$3,context_window_tokens=$4,weekly_after_permille=(select weekly_used_permille from provider_limits where provider=runs.provider),exact=exact and $5 where input_command_id=$1 and finished_at is null")
        .bind(command).bind(duration).bind(number(fact.context_tokens)?).bind(number(fact.context_window_tokens)?).bind(now - at <= EXACT_WINDOW).execute(&mut *conn).await?.rows_affected();
    if changed == 0 {
        if fact.adopted {
            add_adopted(conn, command, fact).await?;
        }
        return Ok(());
    }
    for usage in &fact.usage {
        sqlx::query("insert into run_model_usage(input_command_id,model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens) values($1,$2,$3,$4,$5,$6,$7) on conflict(input_command_id,model,agent) do update set input_tokens=excluded.input_tokens,output_tokens=excluded.output_tokens,cache_read_tokens=excluded.cache_read_tokens,cache_write_tokens=excluded.cache_write_tokens")
            .bind(command).bind(&usage.model).bind(if usage.agent.is_empty(){"main"}else{&usage.agent}).bind(number(usage.input_tokens)?).bind(number(usage.output_tokens)?).bind(number(usage.cache_read_tokens)?).bind(number(usage.cache_write_tokens)?).execute(&mut *conn).await?;
    }
    if let Some(provider) = provider {
        refresh_overlap(conn, provider, started).await?;
    }
    Ok(())
}

// Ход уже закончен, а запуск без ввода тратит лимит дальше: его расход идёт к той же команде,
// контекст обновляется, длительность хода остаётся.
async fn add_adopted(
    conn: &mut PgConnection,
    command: Uuid,
    fact: &RunFinished,
) -> Result<(), sqlx::Error> {
    if fact.context_tokens > 0 {
        sqlx::query(
            "update runs set context_tokens=$2,context_window_tokens=$3 where input_command_id=$1",
        )
        .bind(command)
        .bind(number(fact.context_tokens)?)
        .bind(number(fact.context_window_tokens)?)
        .execute(&mut *conn)
        .await?;
    }
    for usage in &fact.usage {
        sqlx::query("insert into run_model_usage(input_command_id,model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens) values($1,$2,$3,$4,$5,$6,$7) on conflict(input_command_id,model,agent) do update set input_tokens=run_model_usage.input_tokens+excluded.input_tokens,output_tokens=run_model_usage.output_tokens+excluded.output_tokens,cache_read_tokens=run_model_usage.cache_read_tokens+excluded.cache_read_tokens,cache_write_tokens=run_model_usage.cache_write_tokens+excluded.cache_write_tokens")
            .bind(command).bind(&usage.model).bind(if usage.agent.is_empty(){"main"}else{&usage.agent}).bind(number(usage.input_tokens)?).bind(number(usage.output_tokens)?).bind(number(usage.cache_read_tokens)?).bind(number(usage.cache_write_tokens)?).execute(&mut *conn).await?;
    }
    Ok(())
}

// Строки провайдера, чей интервал кончается позже `since`, блокируются в одном порядке.
async fn lock_provider_runs(
    conn: &mut PgConnection,
    provider: &str,
    since: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query("select input_command_id from runs where provider=$1 and coalesce(finished_at,'infinity')>$2 order by input_command_id for update")
        .bind(provider).bind(since).fetch_all(conn).await?;
    Ok(())
}

// Незавершённый ход считается открытым; затрагиваются только строки, кончающиеся позже `since`.
async fn refresh_overlap(
    conn: &mut PgConnection,
    provider: &str,
    since: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    lock_provider_runs(conn, provider, since).await?;
    sqlx::query("with calc as (select a.input_command_id id, exists(select 1 from runs b where b.provider=a.provider and b.input_command_id<>a.input_command_id and a.started_at<coalesce(b.finished_at,'infinity') and b.started_at<coalesce(a.finished_at,'infinity')) o from runs a where a.provider=$1 and coalesce(a.finished_at,'infinity')>$2) update runs r set overlapped=calc.o from calc where r.input_command_id=calc.id and r.overlapped is distinct from calc.o")
        .bind(provider).bind(since).execute(conn).await?;
    Ok(())
}

fn number(value: u64) -> Result<i64, sqlx::Error> {
    i64::try_from(value).map_err(|_| sqlx::Error::InvalidArgument("run counter overflow".into()))
}
