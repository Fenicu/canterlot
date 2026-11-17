-- Версии CLI в отчёте машины: только показ в PWA, хаб не обновляет установленные CLI.
-- Пустые строки по умолчанию оставляют старые отчёты и воркеры без этой возможности рабочими.
alter table machine_harnesses
  add column installed_version text not null default '',
  add column latest_version text not null default '',
  add column min_version text not null default '',
  add column update_command text not null default '';
