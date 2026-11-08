-- Сторож машины: последний отчёт воркера (крупные каталоги сборки, пережившие
-- исполнение процессы, сборки вне очереди), серия выборок с малой памятью,
-- эпизоды памяти и утечек и отметки доставленных событий машины по получателю.
create table machine_watch (
  machine_id uuid primary key references machines(id) on delete cascade,
  owner_id uuid not null references owners(id),
  hotspots jsonb not null default '[]',
  leaked jsonb not null default '[]',
  unleased_count integer not null default 0,
  -- Время отчёта; пусто, пока воркер с watchdog_v1 ничего не прислал.
  observed_at timestamptz,
  -- Подряд идущие выборки метрик со свободной памятью ниже порога.
  mem_low_samples integer not null default 0,
  -- Время последней учтённой выборки: обход не считает одну выборку дважды.
  mem_sample_at timestamptz
);

create table machine_watch_episodes (
  id uuid primary key,
  machine_id uuid not null references machines(id) on delete cascade,
  owner_id uuid not null references owners(id),
  kind text not null check (kind in ('mem_low', 'processes_leaked')),
  started_at timestamptz not null,
  ended_at timestamptz
);
create unique index machine_watch_episodes_open on machine_watch_episodes (machine_id, kind)
  where ended_at is null;

-- Один получатель слышит событие эпизода одного вида один раз. Эпизод — диска
-- или сторожа, поэтому внешнего ключа на эпизод нет.
create table machine_event_marks (
  episode_id uuid not null,
  kind text not null,
  conversation_id uuid not null references conversations(id) on delete cascade,
  created_at timestamptz not null default now(),
  primary key (episode_id, kind, conversation_id)
);
create index machine_event_marks_conversation on machine_event_marks (conversation_id);
