-- Очередь запусков получила две транзиентные причины: предел одновременно
-- работающих детей родителя и паузу между их запусками.
alter table spawn_queue drop constraint spawn_queue_reason_check;
alter table spawn_queue add constraint spawn_queue_reason_check
    check (reason in ('rate', 'concurrency', 'limits', 'resources', 'children', 'stagger'));
