-- Доска плана оркестратора (pilot.md §12.1 п. 4): задачи живут в хабе и переживают
-- сжатие контекста и перезапуск. Хранится только текст оркестратора; модель, коммиты,
-- отчёт и состояние исполнителя выводятся при чтении из его разговора и `runs`.

-- Счётчик номеров на оркестратора: номер не переиспользуется и после переноса задач.
create table plan_boards (
    orchestrator_conversation_id uuid primary key references conversations(id) on delete cascade,
    owner_id uuid not null,
    next_number integer not null default 1 check (next_number >= 1),
    updated_at timestamptz not null default clock_timestamp()
);

create table plan_tasks (
    id uuid primary key,
    owner_id uuid not null,
    orchestrator_conversation_id uuid not null references conversations(id) on delete cascade,
    number integer not null check (number >= 1),
    title text not null check (char_length(title) between 1 and 200),
    note text check (octet_length(note) <= 2048),
    blocker text check (char_length(blocker) <= 500),
    findings jsonb not null default '[]'::jsonb check (jsonb_typeof(findings) = 'array'),
    state text not null default 'waiting'
        check (state in ('waiting', 'in_progress', 'in_review', 'done', 'cancelled', 'blocked')),
    executor_conversation_id uuid references conversations(id) on delete set null,
    linked_at timestamptz,
    attempts integer not null default 0 check (attempts >= 0),
    -- Отпечаток устоявшегося состояния исполнителя, уже переведшего задачу на проверку:
    -- тот же эпизод не возвращает её на проверку после ручной правки.
    review_fingerprint text,
    created_at timestamptz not null default clock_timestamp(),
    updated_at timestamptz not null default clock_timestamp(),
    revision bigint not null default 1,
    unique (orchestrator_conversation_id, number)
);

create index plan_tasks_executor on plan_tasks (executor_conversation_id);
create index plan_tasks_owner_updated on plan_tasks (owner_id, updated_at desc);
