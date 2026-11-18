-- Заготовки сообщений владельца: глобальные и привязанные к рабочей папке проекта.
-- Проектом служит канонический workdir сессии: отдельного идентификатора проекта в хабе нет.
create table message_snippets (
  id uuid primary key,
  owner_id uuid not null references owners(id),
  scope text not null check (scope in ('global', 'project')),
  project_key text not null default '',
  title text not null check (char_length(title) between 1 and 80),
  body text not null check (octet_length(body) between 1 and 262144),
  position integer not null default 0,
  revision bigint not null default 1,
  created_at timestamptz not null default clock_timestamp(),
  updated_at timestamptz not null default clock_timestamp(),
  deleted_at timestamptz,
  check (scope = 'global' or project_key <> '')
);

create unique index message_snippets_title
  on message_snippets (owner_id, scope, project_key, lower(title))
  where deleted_at is null;

create index message_snippets_order
  on message_snippets (owner_id, scope, project_key, position)
  where deleted_at is null;
