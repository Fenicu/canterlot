-- Лента субагента обходит потомков вызова рекурсивно; без индекса каждый уровень читает все блоки разговора.
create index blocks_parent on blocks (parent_block_id) where parent_block_id is not null;
