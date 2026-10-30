-- Передача решения агенту могла состояться, но доказательства применения нет.
alter table requests drop constraint requests_state_check;
alter table requests add constraint requests_state_check check (state in (
  'open',
  'decision_saved',
  'decision_transmitting',
  'decision_applied',
  'decision_unconfirmed',
  'decision_not_applied',
  'closed_without_decision'
));
