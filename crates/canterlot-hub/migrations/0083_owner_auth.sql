-- Собственный вход владельца (ADR-0005). Секреты хранятся только как SHA-256 над
-- префиксом вида секрета и самим секретом; пароль хранится строкой PHC (argon2id).

-- Пароль владельца: одна строка на владельца, без строки пароля нет.
create table owner_passwords (
  owner_id uuid primary key references owners(id),
  phc text not null,
  changed_at timestamptz not null default clock_timestamp()
);

-- Настройки входа: срок бездействия сеанса в днях. Без строки действует 90.
create table owner_auth_settings (
  owner_id uuid primary key references owners(id),
  idle_timeout_days integer not null default 90
    check (idle_timeout_days in (7, 30, 90, 180, 365)),
  updated_at timestamptz not null default clock_timestamp()
);

-- Сеансы браузера. Отозванная строка живёт 30 дней, затем остаётся надгробие.
-- approved_by: сеанс, подтвердивший вход по QR; по нему идёт каскадный отзыв.
create table owner_sessions (
  id uuid primary key,
  owner_id uuid not null references owners(id),
  token_sha256 bytea not null unique check (octet_length(token_sha256) = 32),
  created_at timestamptz not null default clock_timestamp(),
  last_seen_at timestamptz not null default clock_timestamp(),
  authenticated_at timestamptz,
  method text not null check (method in ('password', 'passkey', 'qr', 'recovery')),
  approved_by uuid references owner_sessions(id) on delete set null,
  created_ip inet,
  last_ip inet,
  user_agent text check (octet_length(user_agent) <= 512),
  client_hints jsonb check (octet_length(client_hints::text) <= 2048),
  name text check (char_length(name) between 1 and 64),
  created_location jsonb,
  last_location jsonb,
  revoked_at timestamptz,
  revoke_reason text check (revoke_reason in
    ('logout', 'owner', 'others', 'password_changed', 'idle', 'server', 'limit', 'cascade')),
  check ((revoked_at is null) = (revoke_reason is null)),
  check (method = 'qr' or approved_by is null)
);
create index owner_sessions_live on owner_sessions(owner_id, last_seen_at desc)
  where revoked_at is null;
create index owner_sessions_approved_by on owner_sessions(approved_by);
create index owner_sessions_revoked on owner_sessions(revoked_at) where revoked_at is not null;

-- Надгробие отозванного сеанса: потерянное устройство через месяцы всё ещё
-- получает session_revoked и стирает свои данные. Хранится 400 дней.
create table owner_session_tombstones (
  token_sha256 bytea primary key check (octet_length(token_sha256) = 32),
  revoked_at timestamptz not null,
  revoke_reason text not null
);
create index owner_session_tombstones_revoked on owner_session_tombstones(revoked_at);

-- Известные устройства: cookie ничего не удостоверяет, только выводит попытки
-- пароля из-под общей блокировки и даёт им свою корзину.
create table owner_devices (
  token_sha256 bytea primary key check (octet_length(token_sha256) = 32),
  owner_id uuid not null references owners(id),
  created_at timestamptz not null default clock_timestamp(),
  last_used_at timestamptz not null default clock_timestamp()
);
create index owner_devices_owner on owner_devices(owner_id);
create index owner_devices_last_used on owner_devices(last_used_at);

-- Ссылка восстановления: одна на владельца, новая заменяет прежнюю, используется один раз.
create table owner_recovery_tokens (
  owner_id uuid primary key references owners(id),
  token_sha256 bytea not null unique check (octet_length(token_sha256) = 32),
  created_at timestamptz not null default clock_timestamp(),
  expires_at timestamptz not null
);

-- Корзины перебора. Ключ: ip:<v4>, net:<v6>/64, device:<hex>, unknown, global.
-- Попытка резервируется заранее (failures + 1), успех очищает корзину.
create table auth_buckets (
  key text primary key,
  failures integer not null default 0 check (failures >= 0),
  last_failure_at timestamptz,
  locked_until timestamptz
);
create index auth_buckets_last_failure on auth_buckets(last_failure_at);

-- Журнал подтверждённых неудач: общая блокировка и сводный пуш считают по нему.
create table auth_failures (
  id bigint generated always as identity primary key,
  bucket text not null,
  at timestamptz not null default clock_timestamp()
);
create index auth_failures_at on auth_failures(at);
