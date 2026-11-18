-- Повторяющиеся запуски владельца: расписания и журнал их попыток.
-- Ревизия строки защищает правку от устаревшего редактора, как у черновиков и пресетов.
-- Пара (расписание, момент) уникальна: параллельные тики и перезапуск не удвоят запуск.
create table schedules (
  id uuid primary key,
  owner_id uuid not null references owners(id),
  name text not null,
  enabled boolean not null default true,
  cron text not null,
  timezone text not null,
  target jsonb not null check (jsonb_typeof(target) = 'object'),
  prompt text not null,
  precheck text,
  precheck_timeout_seconds integer not null default 60
    check (precheck_timeout_seconds between 1 and 300),
  catch_up_minutes integer not null default 30
    check (catch_up_minutes between 0 and 1440),
  overlap text not null default 'skip' check (overlap in ('skip','allow')),
  title_template text not null default '{name} · {date}',
  next_fire_at timestamptz not null,
  created_at timestamptz not null default clock_timestamp(),
  updated_at timestamptz not null default clock_timestamp(),
  revision bigint not null default 1
);
create index schedules_owner_updated on schedules (owner_id, updated_at desc);
create index schedules_due on schedules (next_fire_at) where enabled;

create table schedule_runs (
  id uuid primary key,
  schedule_id uuid not null references schedules(id) on delete cascade,
  owner_id uuid not null references owners(id),
  scheduled_at timestamptz not null,
  claimed_revision bigint not null,
  state text not null check (state in (
    'claimed','prechecking','started','skipped_precheck','skipped_limits',
    'skipped_overlap','skipped_machine_offline','skipped_changed','missed','failed'
  )),
  started_at timestamptz,
  delay_ms bigint,
  -- Идентификатор сессии исторический: сессия может быть удалена, журнал живёт дальше.
  session_id uuid,
  input_id uuid,
  manual boolean not null default false,
  run_anyway boolean not null default false,
  -- Владение приёмкой: кто вправе двигать строку дальше. Восстановление
  -- забирает его атомарно (compare-and-set), возраст аренды меряется
  -- от последней смены владельца, а не от создания заявки.
  attempt_id uuid,
  lease_at timestamptz,
  idempotency_key text not null,
  detail jsonb not null default '{}'::jsonb,
  created_at timestamptz not null default clock_timestamp(),
  updated_at timestamptz not null default clock_timestamp(),
  unique (schedule_id, scheduled_at)
);
create index schedule_runs_schedule_time on schedule_runs (schedule_id, scheduled_at desc);
create index schedule_runs_owner_state on schedule_runs (owner_id, state, created_at desc);

-- Резерв ручного запуска: один ключ клиента — один запуск владельца.
-- Уникальность только у ручных строк: ключи срабатываний по расписанию
-- (`schedule:<id>:<момент>`) устроены по правилу, которое клиент может
-- повторить, и общий индекс останавливал бы пачку заявок тика.
-- Срабатывания держатся на паре (расписание, момент) выше.
-- Основной резерв — запись общих ключей идемпотентности в той же транзакции,
-- что и строка запуска: она переживает уборку журнала запусков.
create unique index schedule_runs_owner_manual_key
  on schedule_runs (owner_id, idempotency_key) where manual;

-- Долговечный ограничитель пушей о несработавших запусках: не чаще раза
-- в час на расписание, переживает перезапуск (в отличие от проверки
-- по времени постановки в owner_pushes).
alter table schedules add column last_failure_push_at timestamptz;

-- Пуш о несработавшем расписании не привязан к разговору: чаще всего
-- сессии нет (ошибка до dispatch). Ограничение «не чаще раза в час»
-- держит код постановки, а не ключ: ключ уникален на запуск.
alter table owner_pushes drop constraint owner_pushes_kind_check;
alter table owner_pushes add constraint owner_pushes_kind_check check (
  (kind in ('waiting', 'turn_finished', 'failure') and conversation_id is not null)
  or (kind in ('sign_in', 'sign_in_failures', 'sign_in_lock', 'session_country',
               'credential_expiry', 'credential_auth', 'schedule_failed')
      and conversation_id is null)
);
