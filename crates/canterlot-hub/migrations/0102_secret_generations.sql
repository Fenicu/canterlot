-- Поколения секретных запросов и обязательства отзыва.
--
-- Значения здесь нет и быть не может. Поколение — это монотонный номер выдачи:
-- хаб помнит только текущее, а воркер держит забор `seen`/`dead`, поэтому
-- отозванное поколение не воскресает ни при каком порядке сообщений.
alter table secret_requests add column generation bigint not null default 0;
-- Эпоха хранилища воркера на момент доставки: её смена означает, что tmpfs
-- очищен и файл исчез, даже если строка говорит «доставлен».
alter table secret_requests add column secrets_epoch text;
-- Срок квитанции о доставке: просроченное `sending` становится `lost` и получает
-- обязательство отзыва даже без живой связи.
alter table secret_requests add column send_deadline_at timestamptz;

-- Учет попытки больше не нужен: отзыв называет поколение, а не attempt_id.
drop index if exists secret_requests_attempt;
alter table secret_requests drop column if exists attempt_id;

-- Обязательство отозвать поколение до `up_to` включительно. Строки разговора и
-- запроса намеренно без внешних ключей: окончательная уборка удаляет их, а файл
-- на машине всё равно должен исчезнуть. Каскад только по машине.
create table secret_revocations (
  id uuid primary key,
  machine_id uuid not null references machines(id) on delete cascade,
  conversation_id uuid not null,
  request_id uuid not null,
  up_to bigint not null check (up_to > 0),
  reason text not null,
  attempts integer not null default 0,
  next_attempt_at timestamptz not null default now(),
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(),
  unique (machine_id, request_id)
);

create index secret_revocations_due on secret_revocations(next_attempt_at);
