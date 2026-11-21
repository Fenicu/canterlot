-- Последняя доступность раннера, о которой хаб сообщил клиентам событием
-- `machine_availability_changed` (словарь раннера: online, offline, unknown). У обычной машины
-- остаётся пустой: она живёт на `availability` прежнего словаря.
alter table machines
    add column runner_availability text
        constraint machines_runner_availability_check
        check (runner_availability in ('online', 'offline', 'unknown'));
