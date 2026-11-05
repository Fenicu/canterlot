-- Автопинок: правило на исполнителя и история толчков. Правило ставит агент-предок
-- или владелец; хаб сам решает, когда толкнуть, и отпускает удержанный итог,
-- когда толчки кончились. Доставка идёт заданием session_task, очередь цели
-- здесь не блокируется.
create table nudge_rules (
    conversation_id uuid primary key references conversations(id) on delete cascade,
    owner_id uuid not null references owners(id),
    set_by text not null check (set_by in ('agent', 'owner')),
    set_by_conversation uuid references conversations(id) on delete set null,
    notify_conversation_id uuid references conversations(id) on delete set null,
    enabled boolean not null default true,
    idle_minutes integer not null default 3 check (idle_minutes between 1 and 120),
    background_minutes integer not null default 10 check (background_minutes between 1 and 120),
    max_consecutive integer not null default 3 check (max_consecutive between 1 and 10),
    max_total integer not null default 20 check (max_total between 1 and 100),
    done_markers text[] not null default array['STATUS: DONE', 'STATUS: BLOCKED'],
    text_preset_id text,
    context_warn_percent integer check (context_warn_percent between 50 and 99),
    report_file text,
    classifier jsonb,
    state text not null default 'watching'
        check (state in ('watching', 'pending', 'exhausted', 'paused')),
    held_fingerprint text,
    consecutive integer not null default 0 check (consecutive >= 0),
    total integer not null default 0 check (total >= 0),
    progress_baseline jsonb,
    context_warned_at timestamptz,
    created_at timestamptz not null default clock_timestamp(),
    updated_at timestamptz not null default clock_timestamp(),
    check (cardinality(done_markers) between 1 and 5)
);

-- История толчков: для PWA и разбора качества правил, хранится 30 дней.
create table nudge_events (
    id uuid primary key,
    conversation_id uuid not null references conversations(id) on delete cascade,
    owner_id uuid not null references owners(id),
    at timestamptz not null default clock_timestamp(),
    attempt integer not null default 0,
    signals jsonb not null default '{}',
    outcome text not null
        check (outcome in ('nudged', 'cancelled_state_changed', 'exhausted',
                           'marker_seen', 'asked_in_text')),
    input_id uuid references inputs(id) on delete set null
);
create index nudge_events_conversation on nudge_events (conversation_id, at desc);
create index nudge_events_at on nudge_events (at);

-- Один ожидающий толчок на отпечаток устоявшегося простоя: проектор повторяет
-- оценку каждые 15 секунд и не должен ставить второй.
create unique index scheduled_jobs_nudge_once
    on scheduled_jobs (conversation_id, (payload ->> 'fingerprint'))
    where kind = 'session_task' and state = 'pending'
      and payload ->> 'action' = 'nudge';

-- Внешние ключи на разговоры и вводы: удаление цели и чистка истории не сканируют таблицу.
create index nudge_rules_set_by on nudge_rules (set_by_conversation)
    where set_by_conversation is not null;
create index nudge_rules_notify on nudge_rules (notify_conversation_id)
    where notify_conversation_id is not null;
create index nudge_events_input on nudge_events (input_id)
    where input_id is not null;
