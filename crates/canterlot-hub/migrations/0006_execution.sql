create table blocks (block_id uuid primary key, execution_id uuid not null references executions(id),
  conversation_id uuid not null references conversations(id),
  kind text not null check (kind in ('text','reasoning','tool_call','tool_result','warning','error')),
  state text not null check (state in ('streaming','final','incomplete')),
  rev bigint not null, content text not null default '', checksum text, incomplete_reason text,
  input_command_id uuid references commands(id), parent_block_id uuid,
  created_at timestamptz not null default clock_timestamp(), updated_at timestamptz not null default clock_timestamp());
create index blocks_conversation on blocks (conversation_id, created_at);
alter table executions add column native_conversation_id text, add column phase text, add column phase_at timestamptz, add column observed_at timestamptz;
alter table journal_cursors add column execution_id uuid references executions(id), add column closed boolean not null default false;
alter table commands add column machine_stored_at_ms bigint;
alter table inputs add column transmitted_machine_ms bigint, add column agent_confirmed_machine_ms bigint;
