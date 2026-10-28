create table limit_polls (provider text primary key, last_requested_at timestamptz not null);
-- weekly_reserve_resets_at: сброс недели, в окне которой вошли в резерв; weekly_low_*: подряд идущие новые показания ниже порога выхода.
alter table provider_limits add column weekly_reserve_resets_at timestamptz,
  add column weekly_low_count smallint not null default 0, add column weekly_low_at timestamptz;
-- Лимит остановил сессию в «кранче», а текст продолжения ещё не поставлен в очередь.
alter table conversations add column resume_pending boolean not null default false;
