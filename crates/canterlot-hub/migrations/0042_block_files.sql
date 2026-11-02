-- Файлы, которые агент отдал владельцу: байты лежат в том же blob-хранилище, что вложения,
-- строка появляется только после надёжной публикации; ссылка из блока — по записи BlockFile.
create table block_files (id uuid primary key,
  execution_id uuid not null references executions(id) on delete cascade,
  conversation_id uuid not null references conversations(id) on delete cascade,
  block_id uuid not null,
  attachment_id uuid not null unique references attachments(id) on delete cascade,
  name text not null check (length(name) between 1 and 255),
  mime text not null check (length(mime) between 1 and 127),
  size_bytes bigint not null check (size_bytes between 1 and 52428800),
  sha256 text not null check (sha256 ~ '^[0-9a-f]{64}$'),
  title text not null default '' check (length(title) <= 512),
  note text not null default '' check (length(note) <= 4096),
  width integer check (width > 0), height integer check (height > 0),
  attached_seq bigint, attached_at timestamptz,
  created_at timestamptz not null default clock_timestamp());
create index block_files_block on block_files (block_id, attached_seq) where attached_at is not null;
create index block_files_unreferenced on block_files (created_at) where attached_at is null;
