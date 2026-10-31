-- Команда сжатия контекста живой сессии: только по действию владельца.
alter table commands drop constraint if exists commands_kind_check;
alter table commands add constraint commands_kind_check check (kind in (
  'deliver_input',
  'cancel_input',
  'withdraw_input',
  'interrupt',
  'start_execution',
  'stop_execution',
  'deliver_decision',
  'compact_session'
));
