-- Каталог slash-команд и навыков харнесса: только показ в PWA.
-- Пустой массив по умолчанию оставляет старые отчёты и воркеры без этой возможности рабочими.
alter table machine_harnesses
  add column catalog jsonb not null default '[]'::jsonb;
