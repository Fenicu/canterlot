-- Итоги тестовых прогонов хода: last_run.tests и раздел Tests в итоге подписчику.
alter table runs add column tests jsonb not null default '[]'::jsonb;
