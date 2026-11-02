-- Durable timeline сжатий контекса (авто и по кнопке) и живой расход контекста.
create table compactions (
  id uuid primary key,
  conversation_id uuid not null references conversations(id) on delete cascade,
  execution_id uuid not null references executions(id) on delete cascade,
  trigger text not null check (trigger in ('auto', 'manual')),
  pre_tokens bigint check (pre_tokens is null or pre_tokens >= 0),
  ok boolean,
  created_seq bigint,
  created_at timestamptz not null default now(),
  finished_at timestamptz
);
create index compactions_timeline on compactions (conversation_id, created_seq desc);
-- Последний известный расход контекста исполнения: живые обновления и итог хода.
alter table executions add column context_tokens bigint check (context_tokens is null or context_tokens >= 0),
  add column context_window_tokens bigint check (context_window_tokens is null or context_window_tokens >= 0);
