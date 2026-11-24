-- Облачные окна на Yandex Cloud вместо Timeweb. Провайдер один: машина живёт в каталоге
-- (`folder_id`), её номер и номера операций — строки, подключение идёт по публичному адресу
-- самой машины на порт 22. Роутера, DNAT-правил и пулов адресов больше нет.
--
-- В бою таблица окон пуста, поэтому колонки меняются на месте. Строки тестовых баз
-- переживают миграцию, но их номера Timeweb уже ничего не значат.

alter table cloud_windows rename column server_id to instance_id;
alter table cloud_windows alter column instance_id type text using instance_id::text;

-- Каталог на момент создания окна: удаление, подтверждение и сверка идут в нём, даже если
-- конфигурация хаба успела смениться.
alter table cloud_windows rename column project_id to folder_id;
alter table cloud_windows alter column folder_id type text using coalesce(folder_id::text, '');
alter table cloud_windows alter column folder_id set not null;

alter table cloud_windows
    -- Операция создания: пока она не закончилась, машина не подключается; её ошибка ведёт
    -- окно на снос.
    add column create_operation_id text,
    -- Операция последнего принятого удаления машины.
    add column delete_operation_id text,
    -- Публичный IPv4 машины (one-to-one NAT): адрес подключения несущей.
    add column public_ip text,
    -- Загрузочный диск машины. Он может пережить машину, и тогда тарифицируется дальше.
    add column boot_disk_id text,
    -- Прерываемая машина: снимок на момент создания окна, цены профилей посчитаны под него.
    add column preemptible boolean not null default false,
    -- Профиль, которому звонили последним: снимок перед каждым вызовом создания.
    add column cores integer check (cores > 0),
    add column memory_gb integer check (memory_gb > 0),
    add column disk_gb integer check (disk_gb > 0),
    -- Две ставки часа этого профиля: машина с публичным адресом и диск. Машина платится до
    -- `server_gone_at`, диск — до `disk_gone_at`.
    add column compute_kopecks_per_hour bigint not null default 0
        check (compute_kopecks_per_hour >= 0),
    add column disk_kopecks_per_hour bigint not null default 0
        check (disk_kopecks_per_hour >= 0),
    -- Машина замечена останавливающейся или остановленной (прерывание Яндексом).
    add column stopped_at timestamptz,
    -- Диска окна нет в полном списке дисков каталога.
    add column disk_gone_at timestamptz,
    -- Окно-замена прерванного окна.
    add column replaces_window_id uuid references cloud_windows(id);

alter table cloud_windows alter column preemptible drop default;

-- Вместе с колонкой `dnat_port` уходит и её проверка `cloud_windows_dnat_port_check`.
alter table cloud_windows
    drop column configurator_id,
    drop column local_ip,
    drop column dnat_port,
    drop column dnat_rule_id,
    drop column router_id,
    drop column router_ip,
    drop column dnat_create_attempted_at,
    drop column dnat_delete_accepted_at,
    drop column dnat_gone_at,
    drop column dnat_delete_attempts,
    drop column next_dnat_delete_at;

comment on column cloud_windows.profiles is
    'Профили окна по порядку попыток: [{cores, memory_gb, disk_gb, compute_kopecks_per_hour, disk_kopecks_per_hour}]';
comment on column cloud_windows.price_hourly_kopecks is
    'Сумма двух ставок: до вызова создания у самого дорогого профиля (по ней резерв), после — у профиля, которому звонили';
-- Причины удаления — свободный текст. Новые значения: `preempted` (прерываемую машину
-- остановил Яндекс), `vm_broken` (обычная машина остановилась или сломалась),
-- `create_failed` (создание не дало машины: окно заканчивается `failed`) и `late_disk`
-- (диск законченного окна нашёлся позже).
comment on column cloud_windows.delete_reason is
    'owner, idle, drained, teardown, hard_cap, ready_timeout, overrun, disconnected, gone, late_create, late_disk, create_failed, preempted, vm_broken';

-- У окна не больше одной замены.
create unique index cloud_windows_one_replacement on cloud_windows (replaces_window_id)
    where replaces_window_id is not null;

-- Предложение создаёт одно окно; его замена несёт то же предложение и в правило не входит.
drop index cloud_windows_one_proposal;
create unique index cloud_windows_one_proposal on cloud_windows (proposal_id)
    where proposal_id is not null and replaces_window_id is null;
