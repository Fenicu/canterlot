-- Недельное окно Fable у Claude: отдельная недельная квота модели, которой нет
-- у кадров сессий Opus и Sonnet. Как и месячное, отсутствие окна не отмечается.
alter table provider_limits add column weekly_fable_used_permille integer,
  add column weekly_fable_resets_at timestamptz, add column weekly_fable_observed_at timestamptz;
alter table limit_snapshots drop constraint if exists limit_snapshots_window_kind_check;
alter table limit_snapshots add constraint limit_snapshots_window_kind_check
  check (window_kind in ('five_hour','weekly','monthly','weekly_fable'));
