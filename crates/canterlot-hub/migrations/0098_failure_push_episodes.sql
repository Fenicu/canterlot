-- Один пуш на эпизод сбоя: эпизод — исполнение, состояние и причина. Строка
-- помнит, был ли пуш поставлен и до какого места владелец успел прочитать сессию.
create table failure_push_episodes (
  conversation_id uuid not null references conversations(id) on delete cascade,
  episode text not null,
  read_seq bigint not null,
  pushed boolean not null default false,
  created_at timestamptz not null default clock_timestamp(),
  updated_at timestamptz not null default clock_timestamp(),
  primary key (conversation_id, episode)
);
