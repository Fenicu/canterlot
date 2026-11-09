-- Пуши владельцу через Gotify. Строка настроек есть, только пока в hub.toml задан
-- [notify.gotify]: без неё постановка в очередь ничего не вставляет.
create table push_settings (
  singleton boolean primary key default true check (singleton),
  waiting boolean not null,
  turn_finished boolean not null,
  failures boolean not null,
  timezone text not null default 'UTC',
  updated_at timestamptz not null default clock_timestamp()
);

-- Очередь пушей: вставка в транзакции события, отправка фоновой службой после commit.
-- dedupe_key уникален во всех состояниях: повторная оценка того же эпизода не шлёт снова.
-- Ссылка на разговор без каскада: окончательная уборка удаляет строки явно.
create table owner_pushes (
  id uuid primary key,
  conversation_id uuid not null references conversations(id),
  kind text not null check (kind in ('waiting', 'turn_finished', 'failure')),
  dedupe_key text not null unique,
  payload jsonb not null,
  state text not null default 'pending'
    check (state in ('pending', 'sent', 'dropped', 'cancelled')),
  attempts integer not null default 0 check (attempts >= 0),
  last_status integer,
  run_at timestamptz not null,
  created_at timestamptz not null default clock_timestamp(),
  updated_at timestamptz not null default clock_timestamp()
);
create index owner_pushes_due on owner_pushes(run_at) where state = 'pending';
create index owner_pushes_conversation on owner_pushes(conversation_id);
create index owner_pushes_done on owner_pushes(updated_at) where state <> 'pending';
