alter table commands drop constraint if exists commands_kind_check;
alter table commands add constraint commands_kind_check check (kind in (
  'deliver_input',
  'cancel_input',
  'withdraw_input',
  'interrupt',
  'start_execution',
  'stop_execution',
  'deliver_decision'
));

create table requests (
  id uuid primary key,
  conversation_id uuid not null references conversations(id),
  execution_id uuid not null references executions(id),
  generation bigint not null,
  kind text not null check (kind in ('permission', 'question')),
  native_request_id text not null,
  state text not null check (state in (
    'open',
    'decision_saved',
    'decision_transmitting',
    'decision_applied',
    'decision_not_applied',
    'closed_without_decision'
  )),
  request_payload jsonb not null,
  decision_payload jsonb,
  winner_decision_id uuid,
  idempotency_key text,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);

create index requests_conversation_open on requests(conversation_id, created_at) where state = 'open';
create index requests_conversation on requests(conversation_id, created_at);
create index requests_execution on requests(execution_id);
