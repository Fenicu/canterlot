-- Команда «Отправить сейчас»: влить ожидающий ввод в идущий ход.
alter table commands drop constraint if exists commands_kind_check;
alter table commands add constraint commands_kind_check check (kind in (
  'deliver_input',
  'cancel_input',
  'withdraw_input',
  'interrupt',
  'start_execution',
  'stop_execution',
  'deliver_decision',
  'compact_session',
  'remove_worktree',
  'resolve_owner_lost',
  'steer_input'
));
