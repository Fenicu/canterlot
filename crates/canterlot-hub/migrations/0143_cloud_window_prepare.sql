-- Подготовка облачной машины из одобренной карточки: пакеты, которые ставит cloud-init, и
-- скрипт, который несущая выполняет от root после подъёма выхода в сеть и до запуска воркера.
-- Владелец одобряет подготовку вместе с карточкой как есть; сбой скрипта закрывает окно
-- причиной `prepare_failed`, а хвост вывода остаётся здесь для владельца и агента.
alter table cloud_windows
    add column prepare jsonb check (prepare is null or jsonb_typeof(prepare) = 'object'),
    add column prepare_output text;

comment on column cloud_windows.prepare is
    'Подготовка окна из карточки: {packages?: [string], script?: string}; null — подготовки нет';
comment on column cloud_windows.prepare_output is
    'Хвост вывода скрипта подготовки после сбоя, до 2048 байт; null — сбоя не было';
