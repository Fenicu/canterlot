alter table machines add column availability text not null default 'unavailable' check (availability in ('available','degraded','unavailable')),
  add column availability_at timestamptz, add column capabilities text[] not null default '{}', add column worker_version text,
  add column containment text, add column allowed_roots jsonb not null default '[]', add column reported_at timestamptz,
  add column limits_ok boolean not null default true, add column os text;
create table machine_harnesses (machine_id uuid not null references machines(id) on delete cascade, harness text not null,
  provider text not null, title text not null, images boolean not null default false, settings_midsession boolean not null default false,
  permission_modes jsonb not null default '[]', models jsonb not null default '[]', primary key (machine_id, harness));
create table machine_profiles (machine_id uuid not null references machines(id) on delete cascade, name text not null,
  title text not null, description text not null default '', harnesses jsonb not null default '[]', primary key (machine_id, name));
create table machine_metrics (machine_id uuid primary key references machines(id) on delete cascade,
  observed_at timestamptz not null default clock_timestamp(), machine_time_ms bigint not null, cpu_cores integer not null,
  cpu_permille integer not null, mem_total_bytes bigint not null, mem_available_bytes bigint not null,
  load1_centi integer not null, load5_centi integer not null, load15_centi integer not null, disks jsonb not null, executions jsonb not null);
