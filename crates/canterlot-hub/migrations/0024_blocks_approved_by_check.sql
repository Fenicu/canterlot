-- Единственная отметка воркера: действие разрешено политикой самого CLI без запроса.
update blocks set approved_by = null where approved_by is distinct from 'cli_policy';
alter table blocks add constraint blocks_approved_by_check
  check (approved_by is null or approved_by = 'cli_policy');
