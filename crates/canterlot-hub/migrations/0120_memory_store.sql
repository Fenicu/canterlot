-- Общая память агентов: записи, их прошлые состояния и журнал действий без содержимого.
-- Все запросы к этим таблицам живут в `crate::memory`; сторож в тестах это проверяет.

create table memory_entries (
  id uuid primary key,
  owner_id uuid not null references owners(id),
  domain text not null check (domain in ('work', 'personal')),
  -- NULL: запись всего домена.
  project text,
  kind text not null check (kind in
    ('user', 'feedback', 'project', 'reference', 'decision', 'gotcha', 'environment', 'incident')),
  subject text not null check (char_length(subject) between 1 and 160 and subject !~ E'[\\r\\n]'),
  body text not null check (octet_length(body) between 1 and 8192),
  -- Русская морфология и `simple`: кодовые токены (`ERR_NETWORK_CHANGED`) находятся целиком.
  tsv tsvector generated always as (
    pg_catalog.setweight(
      pg_catalog.to_tsvector('pg_catalog.russian'::pg_catalog.regconfig, subject), 'A')
    || pg_catalog.setweight(
      pg_catalog.to_tsvector('pg_catalog.simple'::pg_catalog.regconfig, subject), 'A')
    || pg_catalog.setweight(
      pg_catalog.to_tsvector('pg_catalog.russian'::pg_catalog.regconfig, body), 'B')
    || pg_catalog.setweight(
      pg_catalog.to_tsvector('pg_catalog.simple'::pg_catalog.regconfig, body), 'B')
  ) stored,
  status text not null default 'active' check (status in ('active', 'archived')),
  valid_until timestamptz,
  version integer not null default 1,
  -- Автор: {"kind":"agent","conversation_id","harness","model"}, {"kind":"owner"}
  -- или {"kind":"import","bundle_id"}.
  created_by jsonb not null,
  created_at timestamptz not null default now(),
  updated_by jsonb not null,
  updated_at timestamptz not null default now(),
  -- {"path","sha256"} у записей из импорта.
  import_source jsonb
);

create unique index memory_entries_active_subject
  on memory_entries (domain, coalesce(project, ''), lower(subject))
  where status = 'active';
create index memory_entries_tsv on memory_entries using gin (tsv);
create index memory_entries_domain_updated on memory_entries (domain, updated_at desc);

-- Состояние записи до изменения; хранятся последние 20 на запись.
create table memory_revisions (
  id bigint generated always as identity primary key,
  entry_id uuid not null references memory_entries(id) on delete cascade,
  version integer not null,
  action text not null check (action in ('update', 'archive', 'restore')),
  project text,
  kind text not null,
  subject text not null,
  body text not null,
  status text not null,
  valid_until timestamptz,
  actor jsonb not null,
  at timestamptz not null default now()
);
create index memory_revisions_entry on memory_revisions (entry_id, id desc);

-- Журнал без содержимого. Переживает жёсткое удаление записи, поэтому без внешнего ключа.
create table memory_events (
  id bigint generated always as identity primary key,
  entry_id uuid not null,
  domain text not null,
  action text not null check (action in
    ('create', 'update', 'archive', 'restore', 'hard_delete')),
  actor jsonb not null,
  -- Разговор автора-агента: по нему считается лимит записей в час.
  conversation_id uuid,
  at timestamptz not null default now()
);
create index memory_events_conversation on memory_events (conversation_id, at)
  where conversation_id is not null;
create index memory_events_entry on memory_events (entry_id, at);
