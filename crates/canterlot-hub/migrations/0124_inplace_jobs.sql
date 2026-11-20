-- Задачи на месте (`job_run`): команда оболочки в каталоге машины с воркером. Одна строка
-- на задачу, попытка пока одна (`attempt`), отдельной таблицы попыток нет.
create table jobs (
  id uuid primary key,
  owner_id uuid not null references owners(id) on delete cascade,
  -- Заказчик: разговор, который вызвал `job_run`. Удалённый разговор оставляет задачу
  -- владельцу, но сообщения об итоге уже не получает.
  conversation_id uuid references conversations(id) on delete set null,
  -- Домен памяти заказчика на момент заявки: смена домена гасит доставку и чтение.
  domain text not null,
  machine_id uuid not null references machines(id),
  -- Целевая машина не машина заказчика: при диспетчеризации нужен выключатель удалённой работы.
  remote boolean not null default false,
  workdir text not null,
  shell text not null,
  env jsonb not null default '[]'::jsonb,
  title text,
  timeout_seconds integer not null check (timeout_seconds between 60 and 86400),
  lease text not null default 'none' check (lease in ('none', 'build', 'gate')),
  state text not null default 'queued' check (state in (
    'queued', 'dispatched', 'waiting_lease', 'running',
    'succeeded', 'failed', 'timed_out', 'canceled', 'lost')),
  attempt integer not null default 1 check (attempt >= 1),
  -- Единственная доставка итога: пусто, пока никто не взял; `inline` отвечает ждущий вызов,
  -- `message` уходит сообщением. Ставится только под замком строки.
  delivery text check (delivery in ('inline', 'message')),
  -- Повтор вызова транспорта и повтор по ключу заказчика возвращают сохранённую задачу.
  call_id uuid,
  idempotency_key text,
  cancel_requested_at timestamptz,
  cancel_reason text,
  -- Причина конца, если она не следует из состояния: `launch_interrupted`,
  -- `workdir_refused`, `remote_work_disabled`, `mode_too_narrow`, `lost`...
  end_reason text,
  exit_code integer,
  duration_ms bigint,
  output_bytes bigint not null default 0,
  truncated boolean not null default false,
  -- Последние 32 КиБ вывода из итога воркера и живой хвост до 64 КиБ из кусков.
  tail bytea,
  live_tail bytea not null default ''::bytea,
  live_offset bigint not null default 0,
  pid bigint,
  proc_start_time bigint,
  -- Полный журнал: gzip в хранилище блобов под этим идентификатором.
  log_media_id uuid,
  log_size bigint,
  log_sha256 text,
  -- Итог воркера, пришедший после `lost`: фиксируется и ничего больше не меняет.
  late_report jsonb,
  created_at timestamptz not null default clock_timestamp(),
  dispatched_at timestamptz,
  started_at timestamptz,
  finished_at timestamptz,
  last_fact_at timestamptz,
  updated_at timestamptz not null default clock_timestamp()
);

create unique index jobs_call on jobs (owner_id, call_id) where call_id is not null;
create unique index jobs_idempotency on jobs (conversation_id, idempotency_key)
  where conversation_id is not null and idempotency_key is not null;
create index jobs_machine_state on jobs (machine_id, state, created_at, id);
create index jobs_conversation on jobs (conversation_id, created_at desc) where conversation_id is not null;
create index jobs_owner_recent on jobs (owner_id, created_at desc, id);
-- Задачи без выбранной доставки: хаб при старте переводит их на сообщение.
create index jobs_undelivered on jobs (id) where delivery is null;

-- Сколько задач на месте машина принимает одновременно (`MachineReport.jobs_max_running`);
-- пусто у воркера без возможности `jobs_inplace_v1`.
alter table machines add column jobs_max_running integer;

-- Команды задач идут тем же outbox: уровня машины, без исполнения.
alter table commands drop constraint if exists commands_kind_check;
alter table commands add constraint commands_kind_check check (kind in (
  'deliver_input','cancel_input','withdraw_input','interrupt','start_execution',
  'stop_execution','deliver_decision','compact_session','remove_worktree',
  'resolve_owner_lost','steer_input','revoke_secret','goal_command',
  'start_inplace_job','cancel_inplace_job'
));
