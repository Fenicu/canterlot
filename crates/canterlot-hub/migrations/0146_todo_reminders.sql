-- Напоминание о списке задач, который агент перестал вести: какую ревизию списка уже
-- называли и когда. Правка этих полей не меняет список и событий не шлёт.
alter table agent_todos add column reminded_revision bigint,
  add column reminded_at timestamptz;
