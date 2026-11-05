-- Маршрутизация C1: политика у владельца, решение у созданной сессии.
-- Очередь запусков (C2) — отдельно.
alter table owners add column routing_policy jsonb;
alter table owners add column routing_policy_revision int not null default 0;
alter table conversations add column routing jsonb;
