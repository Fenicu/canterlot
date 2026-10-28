-- Картинки, на которые смотрел агент: строка появляется только после надёжной публикации blob,
-- ссылка из блока (attached_at) - только по записи BlockMedia журнала сопровождения.
create table block_media (id uuid primary key,
  execution_id uuid not null references executions(id) on delete cascade,
  conversation_id uuid not null references conversations(id) on delete cascade,
  block_id uuid not null,
  mime text not null check (mime in ('image/png','image/jpeg','image/webp','image/gif')),
  size_bytes bigint not null check (size_bytes between 1 and 20971520),
  sha256 text not null check (sha256 ~ '^[0-9a-f]{64}$'),
  width integer check (width > 0), height integer check (height > 0),
  attached_seq bigint, attached_at timestamptz,
  created_at timestamptz not null default clock_timestamp());
create index block_media_block on block_media (block_id, attached_seq) where attached_at is not null;
create index block_media_unreferenced on block_media (created_at) where attached_at is null;
