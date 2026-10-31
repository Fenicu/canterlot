-- Надгробие удалённого разговора: без содержимого, бессрочно, не даёт ему возродиться (§14.1).
create table conversation_tombstones (conversation_id uuid primary key,
  owner_id uuid not null references owners(id), machine_id uuid not null references machines(id),
  max_generation bigint not null, execution_ids uuid[] not null,
  deleted_at timestamptz not null default clock_timestamp());
create index conversation_tombstones_executions on conversation_tombstones using gin (execution_ids);
-- Журнал удалённого исполнения: поздние записи подтверждаются без применения.
alter table journal_cursors add column discarded boolean not null default false;
