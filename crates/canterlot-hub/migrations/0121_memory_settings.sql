-- Настройка владельца «Встроенная память Claude». Строки нет — включена.
create table claude_auto_memory_settings (
  owner_id uuid primary key references owners(id),
  mode text not null check (mode in ('on', 'off')),
  updated_at timestamptz not null default clock_timestamp()
);
