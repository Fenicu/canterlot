alter table conversations add column last_read_seq bigint not null default 0,
  add column last_activity_at timestamptz;
create table conversation_derived (
  conversation_id uuid primary key references conversations(id) on delete cascade,
  activity_state text not null default 'idle' check (activity_state in ('idle','working','waiting_for_user','stalled','quota_paused','failed','stopped')),
  activity_evidence jsonb not null default '{}', activity_since timestamptz not null default clock_timestamp(),
  unread_count integer not null default 0, queue_queued integer not null default 0,
  pause_reasons jsonb not null default '[]', hold_reasons jsonb not null default '[]',
  last_activity_at timestamptz, updated_at timestamptz not null default clock_timestamp());
update conversations c set last_activity_at = date_trunc('minute', greatest(
  (select max(committed_at) from inputs where conversation_id=c.id),
  (select max(created_at) from blocks where conversation_id=c.id)));
create table scheduled_jobs (
  id uuid primary key, kind text not null check (kind in ('message','wake','limit_check','limit_rule')),
  owner_id uuid not null references owners(id), conversation_id uuid references conversations(id) on delete cascade,
  run_at timestamptz not null, state text not null default 'pending' check (state in ('pending','fired','cancelled','failed')),
  payload jsonb not null, attempts integer not null default 0, result jsonb,
  created_at timestamptz not null default clock_timestamp(), updated_at timestamptz not null default clock_timestamp(), fired_at timestamptz);
create index scheduled_jobs_due on scheduled_jobs(run_at) where state='pending';
create index scheduled_jobs_conversation on scheduled_jobs(conversation_id,state);
create unique index scheduled_jobs_wake_once on scheduled_jobs(conversation_id,(payload->>'hold_key')) where kind='wake' and state='pending';
create unique index scheduled_jobs_limit_check_once on scheduled_jobs((payload->>'provider')) where kind='limit_check' and state='pending';
create unique index scheduled_jobs_limit_rule_once on scheduled_jobs((payload->>'provider')) where kind='limit_rule' and state='pending';
