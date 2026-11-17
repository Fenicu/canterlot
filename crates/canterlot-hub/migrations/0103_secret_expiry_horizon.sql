-- Срок, который хаб отправил с поколением: отзыв несёт его воркеру, чтобы
-- решение об уборке забора не зависело от расхождения часов.
alter table secret_requests add column generation_expires_at timestamptz;
alter table secret_revocations
  add column max_expires_at_ms bigint not null default 0;
