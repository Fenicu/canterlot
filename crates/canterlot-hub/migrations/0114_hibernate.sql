-- Усыпление простаивающих сессий: настройка владельца и метка остановки с причиной `hibernated`.
-- Отсутствие строки настроек значит значение по умолчанию, NULL в idle_minutes — «Выключено».
create table hibernate_settings (
  owner_id uuid primary key references owners(id),
  idle_minutes integer check (idle_minutes between 1 and 1440),
  updated_at timestamptz not null default clock_timestamp()
);

alter table executions add column hibernate_requested boolean not null default false;
