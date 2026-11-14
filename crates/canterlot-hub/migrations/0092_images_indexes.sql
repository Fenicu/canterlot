-- Список картинок сессии идёт по номеру области: медиа и файлы блока лежат в тех же
-- индексах, что и лента, поэтому страница не собирается из всей истории разговора.
create index block_media_conversation_seq on block_media (conversation_id, attached_seq) where attached_at is not null;
create index block_files_conversation_seq on block_files (conversation_id, attached_seq) where attached_at is not null;
