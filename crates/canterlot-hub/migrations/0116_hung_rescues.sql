-- Спасение хода, зависшего в генерации: одно действие на эпизод зависания и
-- не больше одного Automatic-действия на ход. Эпизод кончается, когда приходит
-- новое потоковое событие; смена хода обнуляет счётчики.
create table hung_rescues (
  conversation_id uuid primary key references conversations(id) on delete cascade,
  execution_id uuid,
  turn_started_at timestamptz,
  episode_active boolean not null default false,
  rescued boolean not null default false,
  second_notified boolean not null default false,
  updated_at timestamptz not null default clock_timestamp()
);
