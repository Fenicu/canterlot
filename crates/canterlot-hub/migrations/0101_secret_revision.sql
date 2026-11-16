-- Версия строки секретного запроса: PWA сравнивает ответы и события по ней,
-- а не по часам, и старый ответ не затирает более новое состояние.
alter table secret_requests add column revision bigint not null default 0;
