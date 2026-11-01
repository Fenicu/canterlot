-- Межсессионные инструменты: белый список запуска, связь родитель-потомок, рабочие копии,
-- происхождение вводов, git-факты хода и подписки на завершение чужой сессии.
alter table owners add column spawn_whitelist jsonb;

alter table conversations
  add column parent_conversation_id uuid references conversations(id),
  add column spawn_depth integer not null default 0 check (spawn_depth >= 0),
  add column worktree jsonb;
create index conversations_parent on conversations(parent_conversation_id)
  where parent_conversation_id is not null;

alter table inputs add column origin jsonb;
create index inputs_agent_origin on inputs((origin->>'from'), received_at)
  where origin is not null;

alter table runs add column git jsonb;

alter table machines add column worktree_root text;

create table session_subscriptions (
  id uuid primary key,
  owner_id uuid not null references owners(id),
  subscriber_id uuid not null references conversations(id),
  target_id uuid not null references conversations(id),
  chain_depth integer not null default 0,
  last_fingerprint text,
  armed_at timestamptz not null default clock_timestamp(),
  closed_at timestamptz,
  closed_reason text,
  unique (subscriber_id, target_id)
);
create index session_subscriptions_target on session_subscriptions(target_id)
  where closed_at is null;

alter table scheduled_jobs drop constraint scheduled_jobs_kind_check;
alter table scheduled_jobs add constraint scheduled_jobs_kind_check
  check (kind in ('message', 'wake', 'limit_check', 'limit_rule', 'session_task'));

alter table commands drop constraint if exists commands_kind_check;
alter table commands add constraint commands_kind_check check (kind in (
  'deliver_input',
  'cancel_input',
  'withdraw_input',
  'interrupt',
  'start_execution',
  'stop_execution',
  'deliver_decision',
  'compact_session',
  'remove_worktree'
));

alter table requests drop constraint requests_kind_check;
alter table requests add constraint requests_kind_check
  check (kind in ('permission', 'question', 'spawn_approval'));
