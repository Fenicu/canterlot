-- Цель разговора: последнее состояние, пришедшее от адаптера. События уходят
-- на область владельца, чтобы все устройства видели цель в шапке чата сразу.
alter table machine_harnesses add column goals boolean not null default false;

create table conversation_goals (
  conversation_id uuid primary key references conversations(id) on delete cascade,
  generation bigint not null,
  objective text not null,
  status text not null check (status in (
    'active', 'paused', 'blocked', 'usage_limited', 'budget_limited', 'complete'
  )),
  tokens_used bigint not null default 0,
  time_used_seconds bigint not null default 0,
  token_budget bigint,
  turns_used bigint,
  updated_at timestamptz not null default clock_timestamp()
);

-- Команда управления целью идёт тем же outbox, что и сжатие и вливание ввода.
alter table commands drop constraint if exists commands_kind_check;
alter table commands add constraint commands_kind_check check (kind in (
  'deliver_input','cancel_input','withdraw_input','interrupt','start_execution',
  'stop_execution','deliver_decision','compact_session','remove_worktree',
  'resolve_owner_lost','steer_input','revoke_secret','goal_command'
));
