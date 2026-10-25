create table owners (id uuid primary key, created_at timestamptz not null default now());
create table machines (id uuid primary key, owner_id uuid not null references owners(id), name text not null,
  created_at timestamptz not null default now());
create table conversations (id uuid primary key, owner_id uuid not null references owners(id),
  machine_id uuid not null references machines(id), harness text not null, workdir text not null,
  title text not null default '', created_at timestamptz not null default now());
create table executions (id uuid primary key, conversation_id uuid not null references conversations(id),
  generation bigint not null,
  state text not null check (state in ('created','starting','running','stopping','stopped','failed','owner_lost')),
  outcome text, created_at timestamptz not null default now(), updated_at timestamptz not null default now(),
  unique (conversation_id, generation));
create unique index executions_one_live on executions(conversation_id) where state not in ('stopped','failed');
create table queue_state (conversation_id uuid primary key references conversations(id),
  pause_reasons jsonb not null default '[]', hold_reasons jsonb not null default '[]',
  run_slot text not null default 'free' check (run_slot in ('free','dispatching','running')),
  run_slot_input_id uuid, run_slot_attempt integer,
  next_input_seq bigint not null default 1, closed boolean not null default false,
  check ((run_slot = 'free') = (run_slot_input_id is null)));
create table commands (id uuid primary key, machine_id uuid not null references machines(id),
  conversation_id uuid references conversations(id), execution_id uuid references executions(id),
  execution_generation bigint,
  kind text not null check (kind in ('deliver_input','cancel_input','withdraw_input','interrupt','start_execution','stop_execution')),
  payload jsonb not null, target_command_id uuid references commands(id),
  delivery_state text not null check (delivery_state in ('held','pending','stored_on_machine','completed')),
  dispatch_attempt integer not null default 0, last_status text, last_status_rank integer not null default 0,
  result jsonb, created_at timestamptz not null default now(), stored_at timestamptz, completed_at timestamptz);
create index commands_outbox on commands(machine_id, created_at) where delivery_state = 'pending';
create table inputs (id uuid primary key, conversation_id uuid not null references conversations(id),
  owner_id uuid not null references owners(id), input_seq bigint not null, idempotency_key text not null,
  content jsonb not null,
  state text not null check (state in ('queued','dispatched','on_machine','transmitting','accepted','acceptance_unconfirmed','cancelled','rejected')),
  cancel_requested boolean not null default false, reject_reason text, retry_of uuid references inputs(id),
  command_id uuid not null unique references commands(id),
  received_at timestamptz not null, committed_at timestamptz not null default now(), agent_confirmed_at timestamptz,
  unique (conversation_id, input_seq));
create index inputs_fifo on inputs(conversation_id, input_seq) where state = 'queued';
create table idempotency_keys (owner_id uuid not null references owners(id), key text not null,
  operation text not null, request_hash bytea not null, response jsonb not null,
  conversation_id uuid references conversations(id), created_at timestamptz not null default now(),
  primary key (owner_id, key));
create table scope_counters (scope text primary key, last_seq bigint not null);
create table events (scope text not null, scope_seq bigint not null, id uuid not null unique, kind text not null,
  conversation_id uuid, execution_id uuid, payload jsonb not null, correlation_id uuid not null,
  committed_at timestamptz not null default now(), primary key (scope, scope_seq));
