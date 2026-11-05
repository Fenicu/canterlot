-- Пороги лимитов владельца: окраска остатка и недельный резерв. Строки нет — значения
-- по умолчанию из [limits] hub.toml; `reserve_remaining_pct = 0` отключает резерв.
create table limit_settings (
  owner_id uuid primary key references owners(id),
  warn_remaining_pct smallint not null check (warn_remaining_pct between 0 and 99),
  danger_remaining_pct smallint not null check (danger_remaining_pct between 0 and 99),
  reserve_remaining_pct smallint not null check (reserve_remaining_pct between 0 and 99),
  check (danger_remaining_pct <= warn_remaining_pct),
  updated_at timestamptz not null default clock_timestamp()
);
