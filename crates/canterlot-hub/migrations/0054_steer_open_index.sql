-- Диспетчер каждые 5 секунд гасит вливания без итога; частичный индекс
-- избавляет этот запрос от чтения всей таблицы команд.
create index if not exists commands_steer_open on commands (created_at, id)
  where kind = 'steer_input' and delivery_state <> 'completed';
