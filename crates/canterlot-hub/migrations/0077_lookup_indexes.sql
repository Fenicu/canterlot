-- Индексы, которых не хватало удалению разговора и приёму журналов. Курсоры журналов
-- не удаляются (удаление только обнуляет execution_id), поэтому таблица растёт на одну
-- строку на исполнение; ключи идемпотентности живут семь дней. Миграции идут до начала
-- обслуживания, писателей нет, поэтому обычный create index.
create index journal_cursors_execution on journal_cursors (execution_id) where execution_id is not null;
create index idempotency_keys_conversation on idempotency_keys (conversation_id) where conversation_id is not null;
-- Факт решения из CJ ищет запрос по победившему решению.
create index requests_winner_decision on requests (winner_decision_id) where winner_decision_id is not null;
