-- Подсказки следующего сообщения Claude Code. Настройка владельца: строки нет — включены.
create table prompt_suggestion_settings (
  owner_id uuid primary key references owners(id),
  enabled boolean not null,
  updated_at timestamptz not null default clock_timestamp()
);

-- Только последняя подсказка разговора; новый ввод и новый ход её стирают.
create table prompt_suggestions (
  conversation_id uuid primary key references conversations(id) on delete cascade,
  execution_id uuid not null,
  generation bigint not null,
  text text not null,
  created_at timestamptz not null default clock_timestamp()
);
