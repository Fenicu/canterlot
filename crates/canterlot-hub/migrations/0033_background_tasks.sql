-- Фоновые задачи агента: последнее состояние каждой задачи. block_id - вызов Agent или Bash,
-- пара (generation, source_revision) отсекает устаревшее. Счётчик в строке сессии и состояние
-- субагента в делегировании читаются отсюда, поэтому это таблица, а не поле исполнения.
create table background_tasks (
  conversation_id uuid not null references conversations(id) on delete cascade,
  task_id text not null,
  execution_id uuid not null,
  generation bigint not null,
  source_revision bigint not null,
  kind text not null check (kind in ('agent', 'bash')),
  block_id uuid,
  parent_task_id text,
  description text not null,
  state text not null check (state in ('running', 'completed', 'failed', 'stopped')),
  started_at timestamptz not null,
  ended_at timestamptz,
  primary key (conversation_id, task_id)
);
create index background_tasks_block on background_tasks (conversation_id, block_id);

-- Идёт ход, который CLI начал сам: у него нет места в очереди, а модель работает.
alter table executions add column wake_turn boolean not null default false;
