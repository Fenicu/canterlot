-- Эпизод нехватки места: подписка один раз слышит предупреждение и один раз —
-- критический уровень, пока место не вернётся выше порога с запасом.
-- Открытый эпизод один на машину, отметки на подписке переживают перезапуск хаба.
create table machine_disk_episodes (
  id uuid primary key,
  machine_id uuid not null references machines(id) on delete cascade,
  owner_id uuid not null references owners(id),
  started_at timestamptz not null,
  ended_at timestamptz
);
create unique index machine_disk_episodes_open on machine_disk_episodes (machine_id)
  where ended_at is null;
create index machine_disk_episodes_machine on machine_disk_episodes (machine_id, started_at desc);

alter table session_subscriptions
  add column disk_warn_episode uuid references machine_disk_episodes(id) on delete set null,
  add column disk_critical_episode uuid references machine_disk_episodes(id) on delete set null;
