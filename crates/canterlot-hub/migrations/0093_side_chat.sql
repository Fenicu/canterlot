-- Боковой чат: копия сессии, живущая рядом с родителем и не видимая в списках.
-- Удаление идёт приложением, поэтому каскада нет.
alter table conversations add column side_of uuid references conversations(id);
create unique index conversations_side_of on conversations(side_of) where side_of is not null;
-- Граница форка: место родителя, которое копия уже видела; всё позже — после границы.
alter table conversations add column side_boundary jsonb;
-- Свежий форк после потери нативной записи: инструкцию пора повторить.
alter table conversations add column side_reinstruct boolean not null default false;
-- Отчёт о свежем форке этого исполнения; повторный отчёт не повторяет тихую строку.
alter table executions add column fresh_fork boolean not null default false;
-- Свежий перефорк после первого старта: тихая строка в ленте копии.
alter table conversations add column side_fresh_at timestamptz;
