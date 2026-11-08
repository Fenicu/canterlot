-- Зеркало очереди тяжёлых задач машины: последнее полное состояние от воркера
-- (возможность resource_lease_v1), без истории. Время снимка в часах хаба и в
-- часах машины: второе отсекает переставленные снимки одного сеанса. Время приёма
-- не даёт пропаже машины стереть снимок, пришедший уже после переподключения.
-- Пропажа машины очищает состояние, свежее приходит при переподключении.
alter table machines
    add column lease_state jsonb,
    add column lease_observed_at timestamptz,
    add column lease_received_at timestamptz,
    add column lease_machine_ms bigint;

-- Предупреждение о долгом удержании уходит один раз на аренду.
create table lease_long_hold_marks (
    machine_id uuid not null references machines(id) on delete cascade,
    lease_id text not null,
    created_at timestamptz not null default now(),
    primary key (machine_id, lease_id)
);
