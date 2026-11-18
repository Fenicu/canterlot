-- Лента обновлений CLI и каталогов моделей: события пишутся в транзакции отчёта машины.
-- Ключ дедупликации устойчив к перезапуску хаба и повтору того же отчёта.
create table machine_updates (
  id uuid primary key,
  owner_id uuid not null references owners(id) on delete cascade,
  machine_id uuid not null references machines(id) on delete cascade,
  kind text not null check (kind in ('version_available','below_minimum','cli_updated','models_added','models_removed')),
  harness text not null,
  dedup_key text not null unique,
  payload jsonb not null,
  created_at timestamptz not null default clock_timestamp()
);
create index machine_updates_feed on machine_updates(owner_id, created_at desc, id desc);
create index machine_updates_machine on machine_updates(machine_id);
