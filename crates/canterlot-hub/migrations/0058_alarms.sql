-- Будильники оркестратора: разбудить сессию по времени или когда условие станет истинным.
-- Доставка идёт заданием session_task, поэтому таблица хранит только состояние будильника
-- и отметки доставки; сама очередь получателя не блокируется.
create table alarms (
    id uuid primary key,
    owner_id uuid not null,
    conversation_id uuid not null references conversations(id) on delete cascade,
    created_by text not null check (created_by in ('agent', 'owner')),
    key text,
    kind text not null check (kind in ('at', 'condition')),
    condition jsonb,
    fire_at timestamptz,
    deadline_at timestamptz,
    only_on_timeout boolean not null default false,
    batch boolean not null default false,
    collected jsonb not null default '{}',
    note text,
    chain_depth integer not null default 0,
    state text not null default 'armed' check (state in ('armed', 'fired', 'cancelled', 'expired', 'failed')),
    fired_reason text,
    failure_reason text,
    call_id uuid not null,
    response jsonb not null default '{}',
    delivered_input_id uuid,
    delivered_at timestamptz,
    created_at timestamptz not null default clock_timestamp(),
    updated_at timestamptz not null default clock_timestamp(),
    fired_at timestamptz,
    check (
        (kind = 'at' and fire_at is not null and deadline_at is null and condition is null)
        or (kind = 'condition' and fire_at is null and deadline_at is not null and condition is not null)
    )
);

-- Имя будильника уникально среди взведённых одной сессии: повтор по key заменяет прежний.
create unique index alarms_armed_key on alarms (conversation_id, key)
    where state = 'armed' and key is not null;
-- Повтор вызова транспортом отвечает сохранённым ответом.
create unique index alarms_call on alarms (owner_id, call_id);
create index alarms_armed_conversation on alarms (conversation_id) where state = 'armed';
create index alarms_condition_kind on alarms ((condition->>'kind')) where state = 'armed';
create index alarms_condition_sessions on alarms using gin ((condition->'session_ids')) where state = 'armed';
-- Доставка забирает сработавшие и ещё не доставленные будильники получателя.
create index alarms_undelivered on alarms (conversation_id, fired_at)
    where state = 'fired' and delivered_input_id is null;
