-- Список задач агента: зеркало собственного плана харнесса, по строке на дорожку.
-- lane_block_id - блок вызова субагента, у основного агента null. Пара (generation,
-- source_revision) отсекает устаревшее, revision растёт с каждым принятым обновлением
-- и уходит клиенту.
create table agent_todos (
  conversation_id uuid not null references conversations(id) on delete cascade,
  lane_block_id uuid,
  generation bigint not null,
  source_revision bigint not null,
  revision bigint not null,
  items jsonb not null,
  omitted bigint not null check (omitted >= 0),
  updated_at timestamptz not null default clock_timestamp(),
  unique nulls not distinct (conversation_id, lane_block_id)
);
