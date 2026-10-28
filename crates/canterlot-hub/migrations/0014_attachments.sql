-- Вложения хаба: метаданные публикуются только после надёжной публикации содержимого (§13.1).
create table attachments (id uuid primary key, owner_id uuid not null references owners(id),
  conversation_id uuid not null references conversations(id), name text not null check (length(name) between 1 and 255),
  mime text not null default 'application/octet-stream', size_bytes bigint not null check (size_bytes between 1 and 104857600),
  sha256 text not null check (sha256 ~ '^[0-9a-f]{64}$'),
  state text not null default 'created' check (state in ('created','uploading','ready','failed','cancelled','released')),
  fail_reason text, ready_at timestamptz, created_at timestamptz not null default clock_timestamp(), updated_at timestamptz not null default clock_timestamp());
create index attachments_gc on attachments (state, updated_at);
create table input_attachments (input_id uuid not null references inputs(id), attachment_id uuid not null references attachments(id),
  position integer not null, machine_state text not null default 'pending' check (machine_state in ('pending','ready','failed')),
  machine_reason text, retryable boolean, updated_at timestamptz not null default clock_timestamp(),
  primary key (input_id, attachment_id), unique (input_id, position));
create index input_attachments_attachment on input_attachments (attachment_id);
create table scheduled_job_attachments (job_id uuid not null references scheduled_jobs(id) on delete cascade,
  attachment_id uuid not null references attachments(id), primary key (job_id, attachment_id));
create index scheduled_job_attachments_attachment on scheduled_job_attachments (attachment_id);
