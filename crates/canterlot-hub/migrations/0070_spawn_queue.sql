-- Очередь запусков (C2): элемент держит проверенный запрос запуска, пока занята пара,
-- лимит, часовая частота или машина. Задание `session_task` с действием `spawn_dequeue`
-- запускает его тем же путём, что и одобренный запуск.
create table spawn_queue (
    id uuid primary key,
    owner_id uuid not null references owners(id) on delete cascade,
    parent_id uuid not null references conversations(id) on delete cascade,
    call_id uuid not null,
    spec jsonb not null,
    reason text not null check (reason in ('rate', 'concurrency', 'limits', 'resources')),
    detail jsonb not null default '{}'::jsonb,
    state text not null default 'queued'
        check (state in ('queued', 'starting', 'started', 'cancelled', 'failed', 'expired')),
    not_before timestamptz not null,
    expires_at timestamptz not null default clock_timestamp() + interval '24 hours',
    attempts integer not null default 0,
    child_id uuid references conversations(id) on delete set null,
    result jsonb,
    created_at timestamptz not null default clock_timestamp(),
    updated_at timestamptz not null default clock_timestamp()
);

-- Повтор вызова транспортом отвечает тем же элементом очереди.
create unique index spawn_queue_call on spawn_queue (owner_id, call_id);
-- Очередь родителя: ожидающие по времени создания, готовность по сроку.
create index spawn_queue_parent on spawn_queue (parent_id, state, created_at);
create index spawn_queue_due on spawn_queue (state, not_before);
create index spawn_queue_child on spawn_queue (child_id);
