-- Месячное окно лимитов OpenCode: в отличие от пятичасового, отсутствие окна не отмечается.
alter table provider_limits add column monthly_used_permille integer,
  add column monthly_resets_at timestamptz, add column monthly_observed_at timestamptz;
alter table limit_snapshots drop constraint if exists limit_snapshots_window_kind_check;
alter table limit_snapshots add constraint limit_snapshots_window_kind_check
  check (window_kind in ('five_hour','weekly','monthly'));
