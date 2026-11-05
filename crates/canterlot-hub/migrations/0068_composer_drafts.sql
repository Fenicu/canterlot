-- Черновик поля ввода общий для устройств владельца: текст, ревизия и последнее
-- устройство. Пустой текст черновиком не считается, поэтому строка удаляется.
-- Ревизия берётся из общей последовательности: после удаления новая запись
-- получает больший номер, и клиент не принимает старое событие за новое.
create sequence composer_draft_revision_seq;

create table composer_drafts (
    conversation_id uuid primary key references conversations(id) on delete cascade,
    owner_id uuid not null references owners(id),
    text text not null check (octet_length(text) <= 65536),
    revision bigint not null,
    updated_at timestamptz not null default clock_timestamp(),
    device_id text
);

-- Уборка и проверки владельца не сканируют таблицу целиком.
create index composer_drafts_owner on composer_drafts (owner_id);
