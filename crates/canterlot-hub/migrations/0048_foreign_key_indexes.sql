-- Индексы на ссылающихся столбцах: без них проверка внешнего ключа при удалении разговора
-- читает таблицу целиком на каждую удаляемую команду, ввод и исполнение, а горячие пути
-- команд (диспетчер, квитанции, управление) читают всю растущую таблицу commands.
-- Миграции идут до начала обслуживания (main.rs), писателей нет, поэтому обычный
-- create index: на 600 тыс. блоков (heap ~530 МБ) сборка занимает 1-2 с на индекс,
-- на холодном кэше маленького LXC ожидаем до 10-15 с на всю миграцию.
create index blocks_input_command on blocks (input_command_id) where input_command_id is not null;
create index blocks_execution on blocks (execution_id);
create index commands_conversation on commands (conversation_id, kind);
create index commands_execution on commands (execution_id);
create index commands_target on commands (target_command_id) where target_command_id is not null;
create index inputs_retry_of on inputs (retry_of) where retry_of is not null;
create index runs_execution on runs (execution_id);
create index compactions_execution on compactions (execution_id);
create index block_media_conversation on block_media (conversation_id);
create index block_files_conversation on block_files (conversation_id);
create index attachments_conversation on attachments (conversation_id);
create index limit_snapshots_execution on limit_snapshots (execution_id) where execution_id is not null;
-- Очередь снимка: только ещё не попавшие в ленту вводы.
create index inputs_unfed on inputs (conversation_id, input_seq) where feed_seq is null;
