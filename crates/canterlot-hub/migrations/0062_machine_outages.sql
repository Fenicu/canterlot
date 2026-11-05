-- Машина пропала дольше грации: подписка один раз слышит об этом и один раз о возврате.
-- Открытый простой один на машину; подписка помнит, о каком простое её уже предупредили,
-- поэтому повторный обход планировщика и перезапуск хаба не дают второго события.
create table machine_outages (
  id uuid primary key,
  machine_id uuid not null references machines(id) on delete cascade,
  owner_id uuid not null references owners(id),
  started_at timestamptz not null,
  ended_at timestamptz
);
create unique index machine_outages_open on machine_outages (machine_id)
  where ended_at is null;
create index machine_outages_machine on machine_outages (machine_id, started_at desc);

alter table session_subscriptions
  add column machine_unavailable_outage uuid references machine_outages(id) on delete set null,
  add column machine_unavailable_at timestamptz,
  add column machine_available_outage uuid references machine_outages(id) on delete set null;
