-- Предложение владельцу изменить настройку: агент только предлагает, решает владелец.
alter table requests drop constraint requests_kind_check;
alter table requests add constraint requests_kind_check
  check (kind in ('permission', 'question', 'spawn_approval', 'settings_change'));

alter table requests drop constraint requests_state_check;
alter table requests add constraint requests_state_check check (state in (
  'open',
  'decision_saved',
  'decision_transmitting',
  'decision_applied',
  'decision_unconfirmed',
  'decision_not_applied',
  'closed_without_decision',
  'setting_changed',
  'declined',
  'expired'
));

-- Список запуска меняется и через предложение агента: без ревизии нельзя отличить устаревшую карточку.
alter table owners add column spawn_whitelist_revision bigint not null default 0;
