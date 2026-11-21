-- Раннеры как машины: строка `machines` с `role = 'runner'` описывает удалённый хост, на
-- котором воркер запускает несущая машина (carrier) по SSH. Обычные машины остаются `worker`.
-- Раннер хранит родителя (несущую машину), корневой каталог на раннере, ранг размещения
-- (больше — предпочтительнее) и метки. Ранг и метки живут в строке хаба, а не в отчёте несущей.
alter table machines
    add column role text not null default 'worker'
        constraint machines_role_check check (role in ('worker', 'runner')),
    add column parent_machine_id uuid references machines(id),
    add column root_dir text,
    add column rank integer not null default 0,
    add column labels text[] not null default '{}',
    add column is_carrier boolean not null default false,
    add constraint machines_runner_shape_check
        check (role <> 'runner' or (parent_machine_id is not null and root_dir is not null));

create index machines_parent on machines (parent_machine_id) where parent_machine_id is not null;

-- Наблюдения несущей за достижимостью статического раннера: последнее по каждому раннеру.
-- `carrier_id` — кто наблюдал: наблюдение чужой несущей после смены родителя не считается.
create table runner_observations (
    machine_id uuid primary key references machines(id) on delete cascade,
    reachable boolean not null,
    consecutive_failures integer not null check (consecutive_failures >= 0),
    observed_at timestamptz not null,
    carrier_id uuid not null references machines(id)
);
