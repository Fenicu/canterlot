-- Публичный статус провайдера со страницы statuspage: последнее удачное чтение и
-- уровень, о котором уже сообщено клиентам. Действующий уровень — `level`, пока не
-- прошёл `expires_at`, иначе `unknown`.
create table provider_status (
  provider text primary key,
  level text not null
    check (level in ('ok','degraded','maintenance','partial_outage','major_outage','unknown')),
  description text not null default '',
  components jsonb not null default '[]'::jsonb,
  incidents jsonb not null default '[]'::jsonb,
  url text not null,
  fetched_at timestamptz,
  expires_at timestamptz,
  checked_at timestamptz not null,
  failed boolean not null default false,
  announced_level text not null default 'unknown',
  changed_at timestamptz not null
);
