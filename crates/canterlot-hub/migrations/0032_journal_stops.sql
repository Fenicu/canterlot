-- Запись журнала, которую хаб отказался применить: приём журнала стоит, пока курсор не пройдёт seq.
create table journal_stops (
    journal_id uuid primary key,
    machine_id uuid not null references machines(id),
    kind text not null check (kind in ('cj', 'sj')),
    seq bigint not null check (seq > 0),
    reason text not null,
    stopped_at timestamptz not null default clock_timestamp()
);
create index journal_stops_machine on journal_stops (machine_id);
