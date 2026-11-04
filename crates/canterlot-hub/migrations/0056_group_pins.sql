-- Закрепление внутри группы не зависит от глобального: строка живёт, пока сессия
-- состоит в группе. Удаление группы, сессии или членства убирает её каскадом.
create table conversation_group_pins (
  conversation_id uuid not null references conversations(id) on delete cascade,
  group_id uuid not null references conversation_groups(id) on delete cascade,
  pinned_at timestamptz not null default clock_timestamp(),
  primary key (conversation_id, group_id)
);
create index conversation_group_pins_group on conversation_group_pins (group_id, pinned_at desc);
