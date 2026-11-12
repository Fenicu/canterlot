-- Пуши о входе не привязаны к разговору: ссылка становится необязательной,
-- появляются виды входа. Отмена по разговору их не касается: у них нет разговора.
alter table owner_pushes alter column conversation_id drop not null;
alter table owner_pushes drop constraint owner_pushes_kind_check;
alter table owner_pushes add constraint owner_pushes_kind_check check (
  (kind in ('waiting', 'turn_finished', 'failure') and conversation_id is not null)
  or (kind in ('sign_in', 'sign_in_failures', 'sign_in_lock', 'session_country')
      and conversation_id is null)
);
