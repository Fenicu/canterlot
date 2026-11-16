-- Пуш, отложенный из-за открытого чата, не бросается: телефон мог уснуть, так и не
-- прислав «скрыто». Счётчик хранит, сколько раз строку уже откладывали.
alter table owner_pushes add column visible_defers integer not null default 0
  check (visible_defers >= 0);
