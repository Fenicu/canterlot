-- Пуш, подавленный открытым чатом, не отправлен: состояние отличает его от доставки,
-- а текстовый статус называет причину для подсчёта.
alter table owner_pushes drop constraint owner_pushes_state_check;
alter table owner_pushes add constraint owner_pushes_state_check
  check (state in ('pending', 'sent', 'dropped', 'cancelled', 'suppressed'));
alter table owner_pushes alter column last_status type text using last_status::text;
