-- Правка ожидающего ввода: сравнение-и-замена по ревизии.
alter table inputs add column revision bigint not null default 1;
