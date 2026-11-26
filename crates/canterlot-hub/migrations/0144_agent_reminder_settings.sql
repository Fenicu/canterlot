-- Напоминания агентам служебной строкой: об устаревших задачах плана оркестратора и о
-- забытом списке задач. Настройка владельца: строки нет — оба включены.
create table agent_reminder_settings (
  owner_id uuid primary key references owners(id),
  plan_enabled boolean not null,
  todo_enabled boolean not null,
  updated_at timestamptz not null default clock_timestamp()
);
