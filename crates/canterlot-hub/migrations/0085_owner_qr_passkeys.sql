-- Вход по QR, ключи доступа и состояние обрядов WebAuthn (ADR-0005, часть 2).

-- Страны, о которых уже был пуш «используется из новой страны»: страна входа и каждая
-- новая не больше одного раза на сеанс.
alter table owner_sessions add column notified_countries text[] not null default '{}';

-- Вход по QR. Секрет из ссылки и токен опроса хранятся хешами с разными префиксами.
-- delivered: опрос, первым увидевший approved, создал сеанс; второй раз сеанс не выдаётся.
create table owner_qr_logins (
  id uuid primary key,
  owner_id uuid not null references owners(id),
  secret_sha256 bytea not null unique check (octet_length(secret_sha256) = 32),
  poll_sha256 bytea not null unique check (octet_length(poll_sha256) = 32),
  state text not null default 'pending'
    check (state in ('pending', 'scanned', 'approved', 'denied', 'delivered')),
  bucket text not null,
  created_at timestamptz not null default clock_timestamp(),
  expires_at timestamptz not null,
  ip inet,
  user_agent text check (octet_length(user_agent) <= 512),
  client_hints jsonb check (octet_length(client_hints::text) <= 2048),
  location jsonb,
  approved_by uuid references owner_sessions(id) on delete set null,
  approved_at timestamptz,
  session_id uuid references owner_sessions(id) on delete set null,
  check ((state in ('approved', 'delivered')) = (approved_at is not null))
);
create index owner_qr_logins_bucket on owner_qr_logins(bucket, created_at);
create index owner_qr_logins_expires on owner_qr_logins(expires_at);

-- Ключи доступа владельца. passkey: сериализованный webauthn_rs::Passkey (открытый ключ,
-- счётчик, состояние резервной копии); секретов в нём нет, но в журналы он не пишется.
create table owner_passkeys (
  id uuid primary key,
  owner_id uuid not null references owners(id),
  credential_id bytea not null unique,
  passkey jsonb not null,
  name text not null check (char_length(name) between 1 and 64),
  created_at timestamptz not null default clock_timestamp(),
  last_used_at timestamptz
);
create index owner_passkeys_owner on owner_passkeys(owner_id, created_at);

-- Состояние начатого обряда WebAuthn между start и finish: одноразовое, 5 минут.
-- register и reauth привязаны к сеансу, sign_in — к корзине перебора.
create table owner_webauthn_challenges (
  id uuid primary key,
  owner_id uuid not null references owners(id),
  kind text not null check (kind in ('register', 'sign_in', 'reauth')),
  session_id uuid references owner_sessions(id) on delete cascade,
  bucket text,
  state jsonb not null,
  created_at timestamptz not null default clock_timestamp(),
  expires_at timestamptz not null,
  check ((kind = 'sign_in') = (session_id is null))
);
create index owner_webauthn_challenges_expires on owner_webauthn_challenges(expires_at);
create index owner_webauthn_challenges_bucket on owner_webauthn_challenges(bucket, created_at)
  where kind = 'sign_in';
