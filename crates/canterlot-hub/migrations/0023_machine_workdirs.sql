-- Запомненные рабочие каталоги машины: до 20 самых свежих, экран новой
-- сессии показывает последние 3. Удаление каталога на воркере запись не
-- трогает (PWA показывает его приглушённым), хаб файловую систему воркера
-- не проверяет.
create table machine_workdirs (
  owner_id uuid not null references owners(id),
  machine_id uuid not null references machines(id),
  workdir text not null,
  last_used_at timestamptz not null default clock_timestamp(),
  primary key (owner_id, machine_id, workdir)
);
create index machine_workdirs_recent on machine_workdirs
  (owner_id, machine_id, last_used_at desc, workdir desc);
