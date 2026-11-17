-- Конец входа провайдера на машине: дата, которую Claude Code называет в кадре `result`.
-- У даты нет окна и сброса, поэтому она живёт отдельно от `provider_limits`; ключ —
-- машина и провайдер, а не провайдер один: вход свой на каждой машине.
create table provider_credentials (
  machine_id uuid not null references machines(id) on delete cascade,
  provider text not null,
  -- Дата может быть неизвестна: отказ входа бывает и без неё (API-ключ, старый CLI).
  expires_at timestamptz,
  observed_at timestamptz not null default clock_timestamp(),
  -- Дата, о которой владельцу уже сказали пушем: новая дата после повторного входа взводит заново.
  warned_expires_at timestamptz,
  -- Эпизод неудачного входа: пуш один на эпизод, успешный ход после него открывает новый.
  auth_warned_at timestamptz,
  auth_ok_at timestamptz,
  primary key (machine_id, provider)
);

-- Пуши о входе в Claude Code не привязаны к разговору, как и пуши о входе владельца.
alter table owner_pushes drop constraint owner_pushes_kind_check;
alter table owner_pushes add constraint owner_pushes_kind_check check (
  (kind in ('waiting', 'turn_finished', 'failure') and conversation_id is not null)
  or (kind in ('sign_in', 'sign_in_failures', 'sign_in_lock', 'session_country',
               'credential_expiry', 'credential_auth')
      and conversation_id is null)
);
