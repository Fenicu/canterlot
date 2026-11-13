use crate::blocks::ExecutionRef;
use canterlot_proto::worker::v1::{GitFacts, RunFinished};
use chrono::{DateTime, Utc};
use prost::Message;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use uuid::Uuid;

const MAX_GIT_COMMITS: usize = 50;
const MAX_GIT_FILES: usize = 200;
const MAX_GIT_SUBJECT_BYTES: usize = 256;
const MAX_GIT_PATH_BYTES: usize = 1024;
const MAX_GIT_BYTES: usize = 32 * 1024;

/// Форма `runs.git`; тексты коммитов и пути не попадают ни в журнал, ни в `Debug`.
#[derive(Clone, Default, Serialize, Deserialize)]
struct StoredGit {
    #[serde(default)]
    available: bool,
    #[serde(default)]
    head_before: String,
    #[serde(default)]
    head_after: String,
    #[serde(default)]
    dirty: bool,
    #[serde(default)]
    truncated: bool,
    #[serde(default)]
    commits: Vec<StoredCommit>,
    #[serde(default)]
    files: Vec<StoredFile>,
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredCommit {
    sha: String,
    subject: String,
    /// Время коммита в миллисекундах Unix; 0, если старый воркер его не прислал.
    #[serde(default, skip_serializing_if = "is_zero")]
    committed_at_ms: i64,
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

#[derive(Clone, Serialize, Deserialize)]
struct StoredFile {
    path: String,
    status: String,
}

impl From<&GitFacts> for StoredGit {
    fn from(facts: &GitFacts) -> Self {
        Self {
            available: facts.available,
            head_before: facts.head_before.clone(),
            head_after: facts.head_after.clone(),
            dirty: facts.dirty,
            truncated: facts.truncated,
            commits: facts
                .commits
                .iter()
                .map(|c| StoredCommit {
                    sha: c.sha.clone(),
                    subject: c.subject.clone(),
                    committed_at_ms: c.committed_at_ms,
                })
                .collect(),
            files: facts
                .files
                .iter()
                .map(|f| StoredFile {
                    path: f.path.clone(),
                    status: f.status.clone(),
                })
                .collect(),
        }
    }
}

impl StoredGit {
    /// Позднее состояние дописывает коммиты, заменяет конец и подменяет статус файла.
    fn merge(mut self, later: Self) -> Self {
        if !self.available {
            return later;
        }
        if !later.available {
            return self;
        }
        self.head_after = later.head_after;
        self.dirty = later.dirty;
        self.truncated |= later.truncated;
        for commit in later.commits {
            if !self.commits.iter().any(|c| c.sha == commit.sha) {
                self.commits.push(commit);
            }
        }
        for file in later.files {
            match self.files.iter_mut().find(|f| f.path == file.path) {
                Some(known) => known.status = file.status,
                None => self.files.push(file),
            }
        }
        if self.commits.len() > MAX_GIT_COMMITS {
            self.commits.truncate(MAX_GIT_COMMITS);
            self.truncated = true;
        }
        if self.files.len() > MAX_GIT_FILES {
            self.files.truncate(MAX_GIT_FILES);
            self.truncated = true;
        }
        self
    }
}

fn clip_text(text: &mut String, max: usize) -> bool {
    text.retain(|c| c != '\0');
    if text.len() <= max {
        return false;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    true
}

/// Хаб не доверяет воркеру: пределы и NUL (jsonb его не хранит) проверяются заново.
pub fn bound_git(facts: &mut GitFacts) {
    let mut truncated = facts.truncated;
    clip_text(&mut facts.head_before, 64);
    clip_text(&mut facts.head_after, 64);
    if facts.commits.len() > MAX_GIT_COMMITS {
        facts.commits.truncate(MAX_GIT_COMMITS);
        truncated = true;
    }
    if facts.files.len() > MAX_GIT_FILES {
        facts.files.truncate(MAX_GIT_FILES);
        truncated = true;
    }
    for commit in &mut facts.commits {
        clip_text(&mut commit.sha, 64);
        truncated |= clip_text(&mut commit.subject, MAX_GIT_SUBJECT_BYTES);
    }
    for file in &mut facts.files {
        clip_text(&mut file.status, 8);
        truncated |= clip_text(&mut file.path, MAX_GIT_PATH_BYTES);
    }
    facts.truncated = truncated;
    while facts.encoded_len() > MAX_GIT_BYTES {
        facts.truncated = true;
        if facts.files.pop().is_none() && facts.commits.pop().is_none() {
            break;
        }
    }
}

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
    let changed=sqlx::query("update runs set finished_at=started_at+($2::bigint * interval '1 millisecond'),duration_ms=$2,context_tokens=$3,context_window_tokens=$4,weekly_after_permille=(select weekly_used_permille from provider_limits where provider=runs.provider),exact=exact and $5,git=coalesce($6,git),forced_close=false,working_ms=null where input_command_id=$1 and (finished_at is null or forced_close)")
        .bind(command).bind(duration).bind(number(fact.context_tokens)?).bind(number(fact.context_window_tokens)?).bind(now - at <= EXACT_WINDOW).bind(git_json(fact)?).execute(&mut *conn).await?.rows_affected();
    if changed == 0 {
        if fact.adopted {
            add_adopted(conn, command, fact).await?;
        }
        return Ok(());
    }
    store_working(conn, command).await?;
    crate::test_summary::store_in_tx(conn, command).await?;
    for usage in &fact.usage {
        sqlx::query("insert into run_model_usage(input_command_id,model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens) values($1,$2,$3,$4,$5,$6,$7) on conflict(input_command_id,model,agent) do update set input_tokens=excluded.input_tokens,output_tokens=excluded.output_tokens,cache_read_tokens=excluded.cache_read_tokens,cache_write_tokens=excluded.cache_write_tokens")
            .bind(command).bind(&usage.model).bind(if usage.agent.is_empty(){"main"}else{&usage.agent}).bind(number(usage.input_tokens)?).bind(number(usage.output_tokens)?).bind(number(usage.cache_read_tokens)?).bind(number(usage.cache_write_tokens)?).execute(&mut *conn).await?;
    }
    if let Some(provider) = provider {
        refresh_overlap(conn, provider, started).await?;
    }
    Ok(())
}

/// Исполнение закончилось, а конца хода не было (CLI умер посреди хода): открытые ходы
/// закрываются временем конечного перехода и исключаются из точной оценки лимита.
pub async fn close_open_in_tx(
    conn: &mut PgConnection,
    execution_id: Uuid,
) -> Result<(), sqlx::Error> {
    let open: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
        "select provider, min(started_at) from runs
         where execution_id=$1 and finished_at is null and provider is not null group by provider",
    )
    .bind(execution_id)
    .fetch_all(&mut *conn)
    .await?;
    for (provider, since) in &open {
        lock_provider_runs(conn, provider, *since).await?;
    }
    let closed: Vec<Uuid> = sqlx::query_scalar("update runs set finished_at=greatest(started_at,now()),duration_ms=(extract(epoch from (greatest(started_at,now())-started_at))*1000)::bigint,exact=false,forced_close=true where execution_id=$1 and finished_at is null returning input_command_id")
        .bind(execution_id)
        .fetch_all(&mut *conn)
        .await?;
    for command in closed {
        store_working(conn, command).await?;
        crate::test_summary::store_in_tx(conn, command).await?;
    }
    for (provider, since) in &open {
        refresh_overlap(conn, provider, *since).await?;
    }
    Ok(())
}

/// Рабочее время закрытого хода: представление считает его, пока `working_ms` пуст.
async fn store_working(conn: &mut PgConnection, command: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("update runs r set working_ms=w.working_ms from run_working w where w.input_command_id=r.input_command_id and r.input_command_id=$1")
        .bind(command)
        .execute(conn)
        .await?;
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
    merge_git(conn, command, fact).await?;
    crate::test_summary::store_in_tx(conn, command).await?;
    for usage in &fact.usage {
        sqlx::query("insert into run_model_usage(input_command_id,model,agent,input_tokens,output_tokens,cache_read_tokens,cache_write_tokens) values($1,$2,$3,$4,$5,$6,$7) on conflict(input_command_id,model,agent) do update set input_tokens=run_model_usage.input_tokens+excluded.input_tokens,output_tokens=run_model_usage.output_tokens+excluded.output_tokens,cache_read_tokens=run_model_usage.cache_read_tokens+excluded.cache_read_tokens,cache_write_tokens=run_model_usage.cache_write_tokens+excluded.cache_write_tokens")
            .bind(command).bind(&usage.model).bind(if usage.agent.is_empty(){"main"}else{&usage.agent}).bind(number(usage.input_tokens)?).bind(number(usage.output_tokens)?).bind(number(usage.cache_read_tokens)?).bind(number(usage.cache_write_tokens)?).execute(&mut *conn).await?;
    }
    Ok(())
}

fn git_json(fact: &RunFinished) -> Result<Option<serde_json::Value>, sqlx::Error> {
    fact.git
        .as_ref()
        .map(|git| serde_json::to_value(StoredGit::from(git)))
        .transpose()
        .map_err(|e| sqlx::Error::Encode(Box::new(e)))
}

// Приписанный запуск дописывает свои факты к уже сохранённым фактам хода.
async fn merge_git(
    conn: &mut PgConnection,
    command: Uuid,
    fact: &RunFinished,
) -> Result<(), sqlx::Error> {
    let Some(later) = fact.git.as_ref().map(StoredGit::from) else {
        return Ok(());
    };
    let stored: Option<serde_json::Value> =
        sqlx::query_scalar("select git from runs where input_command_id=$1 for update")
            .bind(command)
            .fetch_one(&mut *conn)
            .await?;
    let merged = match stored.and_then(|v| serde_json::from_value::<StoredGit>(v).ok()) {
        Some(known) => known.merge(later),
        None => later,
    };
    let value = serde_json::to_value(merged).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    sqlx::query("update runs set git=$2 where input_command_id=$1")
        .bind(command)
        .bind(value)
        .execute(&mut *conn)
        .await?;
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
