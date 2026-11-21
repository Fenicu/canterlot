-- Удалённая работа: другая машина принимает делегатов и команды чужих сессий только
-- после включения владельцем. Выключатель живёт в строке машины: запуск делегата читает её
-- `for share`, смена берёт `for update`, поэтому после выключения новый запуск не пройдёт.
alter table machines
    add column accepts_remote_work boolean not null default false;

-- Очередь запусков: целевая машина недоступна или не принимает удалённую работу.
alter table spawn_queue drop constraint spawn_queue_reason_check;
alter table spawn_queue add constraint spawn_queue_reason_check
    check (reason in ('rate', 'concurrency', 'limits', 'resources', 'children', 'stagger',
                      'machine_offline', 'remote_work_disabled'));
