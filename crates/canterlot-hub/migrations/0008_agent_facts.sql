alter table blocks add column agent text not null default 'main', add column model text not null default '',
  add column tool_name text, add column summary text, add column created_seq bigint not null default 0,
  add column finished_at timestamptz,
  add column line_count integer;
alter table blocks add column if not exists created_at timestamptz not null default clock_timestamp();
alter table inputs add column created_seq bigint not null default 0,
  add column feed_seq bigint;
alter table inputs add column if not exists created_at timestamptz not null default clock_timestamp();
update inputs set created_at=committed_at;
-- Старые строки без событий располагаются перед реальными scope_seq.
with known as (
 select b.block_id, min(e.scope_seq) seq from blocks b join events e
 on e.conversation_id=b.conversation_id and e.kind='block_started' and e.payload->>'block_id'=b.block_id::text
 group by b.block_id
) update blocks b set created_seq=k.seq, created_at=e.committed_at from known k, events e
 where b.block_id=k.block_id and e.scope='conversation:'||b.conversation_id::text and e.scope_seq=k.seq;
with missing as (
 select block_id, row_number() over(partition by conversation_id order by created_at,block_id)
 - count(*) over(partition by conversation_id)-1 seq from blocks where created_seq=0
) update blocks b set created_seq=m.seq from missing m where m.block_id=b.block_id;
with known as (
 select i.id, min(e.scope_seq) seq from inputs i join events e
 on e.conversation_id=i.conversation_id and e.kind='input_queued' and e.payload->>'input_id'=i.id::text
 group by i.id
) update inputs i set created_seq=k.seq, created_at=e.committed_at from known k, events e
 where i.id=k.id and e.scope='conversation:'||i.conversation_id::text and e.scope_seq=k.seq;
with missing as (
 select id, row_number() over(partition by conversation_id order by created_at,id)
 - count(*) over(partition by conversation_id)-1 seq from inputs where created_seq=0
) update inputs i set created_seq=m.seq from missing m where m.id=i.id;
update inputs i set feed_seq=coalesce((select min(e.scope_seq) from events e
 where e.scope='conversation:'||i.conversation_id::text and e.kind='input_state_changed'
 and e.payload->>'input_id'=i.id::text and e.payload->>'state' in ('transmitting','accepted','acceptance_unconfirmed','rejected')),i.created_seq)
 where i.state in ('transmitting','accepted','acceptance_unconfirmed','rejected');
alter table executions add column phase_detail jsonb, add column last_event_at timestamptz;
create table runs (input_command_id uuid primary key references commands(id),
  execution_id uuid not null references executions(id), conversation_id uuid not null references conversations(id),
  provider text, started_at timestamptz not null default clock_timestamp(), finished_at timestamptz, duration_ms bigint,
  context_tokens bigint, context_window_tokens bigint, weekly_before_permille integer, weekly_after_permille integer,
  overlapped boolean not null default false, exact boolean not null default true);
create index runs_conversation on runs (conversation_id, started_at);
create index blocks_timeline on blocks (conversation_id, created_seq desc);
create index inputs_feed on inputs (conversation_id, feed_seq desc) where feed_seq is not null;
create table run_model_usage (input_command_id uuid not null references runs(input_command_id) on delete cascade,
  model text not null, agent text not null, input_tokens bigint not null, output_tokens bigint not null,
  cache_read_tokens bigint not null, cache_write_tokens bigint not null, primary key (input_command_id, model, agent));
create table limit_snapshots (id bigserial primary key, provider text not null,
  window_kind text not null check (window_kind in ('five_hour','weekly')),
  used_permille integer not null check (used_permille between 0 and 1000), resets_at timestamptz,
  observed_at timestamptz not null,
  received_at timestamptz not null default clock_timestamp(), source text not null check (source in ('session','poll')),
  machine_id uuid references machines(id), execution_id uuid references executions(id));
create index limit_snapshots_lookup on limit_snapshots (provider, window_kind, observed_at desc);
create table provider_limits (provider text primary key,
  weekly_used_permille integer, weekly_resets_at timestamptz, weekly_observed_at timestamptz,
  five_hour_used_permille integer, five_hour_resets_at timestamptz, five_hour_observed_at timestamptz,
  weekly_reserve_active boolean not null default false, updated_at timestamptz not null default clock_timestamp());
