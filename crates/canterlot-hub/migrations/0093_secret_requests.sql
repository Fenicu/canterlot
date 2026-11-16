-- Секретные запросы: одноразовая передача значения владельцем на машину сессии.
-- Значения в этой таблице нет и быть не может: оно живёт только в одном
-- управляющем сообщении, которое не сохраняется ни в outbox, ни в журнале воркера.
create table secret_requests (
  id uuid primary key,
  conversation_id uuid not null references conversations(id) on delete cascade,
  owner_id uuid not null references owners(id),
  machine_id uuid not null references machines(id),
  name text not null check (char_length(name) between 1 and 80),
  purpose text not null check (char_length(purpose) <= 300),
  state text not null check (state in (
    'open', 'sending', 'delivered', 'declined',
    'revoking', 'revoked', 'expired', 'lost', 'cancelled'
  )),
  attempt_id uuid,
  path text,
  expires_at timestamptz,
  note text,
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);

create index secret_requests_conversation on secret_requests(conversation_id, created_at desc);
create index secret_requests_attempt on secret_requests(attempt_id) where attempt_id is not null;
create index secret_requests_machine_sending on secret_requests(machine_id) where state = 'sending';
create index secret_requests_owner_waiting
  on secret_requests(owner_id) where state in ('open', 'lost');

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
  'remove_worktree',
  'resolve_owner_lost',
  'steer_input',
  'revoke_secret'
));
