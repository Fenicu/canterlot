create table installation (singleton boolean primary key default true check (singleton),
  installation_id uuid not null, restore_generation uuid not null, owner_id uuid not null references owners(id),
  created_at timestamptz not null default clock_timestamp());
create table machine_credentials (machine_id uuid primary key references machines(id),
  token_sha256 bytea not null unique, created_at timestamptz not null default clock_timestamp(), revoked_at timestamptz);
create table journal_cursors (machine_id uuid not null references machines(id), journal_id uuid not null,
  kind text not null check (kind in ('cj','sj')), committed_seq bigint not null default 0,
  updated_at timestamptz not null default clock_timestamp(), primary key (machine_id, journal_id));
