alter table machines add column cj_journal_id uuid null;
alter table machine_credentials drop constraint machine_credentials_pkey;
alter table machine_credentials add primary key (token_sha256);
create unique index machine_credentials_active on machine_credentials (machine_id) where revoked_at is null;
