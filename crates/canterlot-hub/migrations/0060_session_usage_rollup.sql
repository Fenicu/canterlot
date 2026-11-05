-- Свёрнутый расход удалённой делегации на её родителя. При удалении сессии её
-- собственные токены и уже свёрнутые расходы её делегатов складываются в строку
-- родителя, поэтому «Делегаты» не уменьшаются, когда принятую сессию убирает срок
-- хранения. Ключ (parent_id, source_id) делает сворачивание идемпотентным, а
-- каскад от родителя убирает строки вместе с ним.
create table conversation_usage_rollup (
  parent_id uuid not null references conversations(id) on delete cascade,
  source_id uuid not null,
  turns bigint not null check (turns >= 0),
  models jsonb not null,
  folded_at timestamptz not null default clock_timestamp(),
  primary key (parent_id, source_id)
);
