-- Привязка машины по одноразовому коду. Владелец заводит машину заранее: она «ожидает
-- подключения», пока воркер не пройдёт первое рукопожатие с выданным за код токеном.
-- Хранится только SHA-256 нормализованного кода: сам код хаб не видит после выдачи.
alter table machines
    add column pending_enrollment boolean not null default false;

create table machine_join_codes (
    machine_id uuid primary key references machines(id) on delete cascade,
    code_sha256 bytea not null unique,
    expires_at timestamptz not null,
    created_at timestamptz not null default now()
);

comment on column machines.pending_enrollment is
    'Машина заведена кодом привязки и ещё не прошла первое рукопожатие';
comment on table machine_join_codes is
    'Живой код привязки ожидающей машины: SHA-256 нормализованного кода, срок 15 минут';
