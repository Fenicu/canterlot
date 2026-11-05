-- Черновик поля ввода общий для устройств владельца: текст, готовые вложения,
-- ревизия и последнее устройство. Пустой черновик (нет ни текста, ни вложений)
-- строкой не хранится. Ревизия берётся из общей последовательности: после
-- удаления новая запись получает больший номер, и клиент не принимает старое
-- событие за новое.
create sequence composer_draft_revision_seq;

create table composer_drafts (
    conversation_id uuid primary key references conversations(id) on delete cascade,
    owner_id uuid not null references owners(id),
    text text not null check (octet_length(text) <= 65536),
    attachment_ids jsonb not null default '[]'::jsonb
        check (jsonb_typeof(attachment_ids) = 'array'
               and jsonb_array_length(attachment_ids) <= 10),
    revision bigint not null,
    updated_at timestamptz not null default clock_timestamp(),
    device_id text
);

-- Уборка и проверки владельца не сканируют таблицу целиком.
create index composer_drafts_owner on composer_drafts (owner_id);
