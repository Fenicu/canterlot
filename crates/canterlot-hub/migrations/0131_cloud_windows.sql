-- Облачные окна (этап C2): жизнь одного облачного сервера раннера от намерения создать до
-- подтверждённого конца оплаты, с резервом и записью расхода в целых копейках.

-- Единственная строка: замок создания окон и настройки денег. Транзакция создания берёт её
-- `for update`, так что два создания не проходят одновременно. Значения, которые владелец
-- меняет (бюджет, автоматический режим), добавляет следующий слой; здесь бюджет месяца
-- (0 — облако создаётся только карточкой с явным согласием на перерасход) и строка машины
-- `cloud`, которую окна переиспользуют.
create table cloud_budget (
    singleton boolean primary key default true check (singleton),
    monthly_budget_kopecks bigint not null default 0 check (monthly_budget_kopecks >= 0),
    cloud_machine_id uuid references machines(id)
);
insert into cloud_budget (singleton) values (true);

create table cloud_windows (
    id uuid primary key,
    -- Имя сервера у провайдера: `canterlot-runner-<id>`. Сверка ищет серверы по нему.
    name text not null unique,
    state text not null check (state in (
        'create_intent', 'create_unknown', 'identified', 'ready', 'draining',
        'deleting', 'ended', 'failed', 'gone')),
    trigger text not null check (trigger in ('card', 'owner', 'auto')),
    proposal_id uuid,
    server_id bigint,
    -- Публичный IPv4: у нового сервера его нет, хаб запрашивает его сразу после создания
    -- (`POST /servers/{id}/ips`), и он живёт дольше сервера: после удаления остаётся на
    -- аккаунте плавающим и тарифицируется, пока его не удалят.
    ipv4 text,
    -- Цена часа адреса на момент создания окна (входит в резерв и в итог).
    ipv4_hourly_kopecks bigint not null default 0 check (ipv4_hourly_kopecks >= 0),
    -- Метка запроса адреса: до самого вызова, второго вызова нет.
    ip_requested_at timestamptz,
    floating_ip_id text,
    -- Сервер подтверждённо пропал из полного списка; адрес ещё может тарифицироваться.
    server_gone_at timestamptz,
    ip_delete_accepted_at timestamptz,
    ip_gone_at timestamptz,
    ip_delete_attempts integer not null default 0 check (ip_delete_attempts >= 0),
    -- Только открытая часть хостового ключа: закрытая уходит лишь в cloud-init.
    host_key_public text not null,
    -- Упорядоченный список профилей на момент создания окна с ценой часа каждого:
    -- `[{configurator_id, cpu, ram_gb, disk_gb, hourly_kopecks}]`. Создание пробует их по
    -- очереди, правка конфигурации хаба в этот момент окну не мешает.
    profiles jsonb not null check (jsonb_typeof(profiles) = 'array' and jsonb_array_length(profiles) > 0),
    -- Конфигуратор профиля, который принял провайдер; пусто, пока сервера нет.
    configurator_id bigint,
    -- Цена часа: до приёма профиля самая дорогая из списка (по ней резерв), после приёма
    -- действительная цена принятого профиля (по ней итог в ledger).
    price_hourly_kopecks bigint not null check (price_hourly_kopecks >= 0),
    reservation_kopecks bigint not null check (reservation_kopecks >= 0),
    -- Пусто, пока окно живо. У окна, у которого сервера не было, 0.
    final_cost_kopecks bigint check (final_cost_kopecks >= 0),
    -- `YYYY-MM` по Москве на момент создания: окно целиком относится к этому месяцу.
    month text not null check (month ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    hard_cap_minutes integer not null check (hard_cap_minutes > 0),
    created_at timestamptz not null,
    -- Метка ставится до вызова API: упавший после неё хаб знает, что сервер мог появиться.
    create_called_at timestamptz,
    ready_at timestamptz,
    -- От вызова создания, а не от готовности.
    hard_cap_at timestamptz,
    drain_until timestamptz,
    -- До какого срока ждём подтверждений отмены, прежде чем задачи станут `lost`.
    teardown_until timestamptz,
    delete_requested_at timestamptz,
    -- Провайдер принял удаление (в том числе в карантин); конец оплаты ещё не подтверждён.
    delete_accepted_at timestamptz,
    delete_confirmed_at timestamptz,
    delete_reason text,
    delete_attempts integer not null default 0 check (delete_attempts >= 0),
    next_delete_at timestamptz,
    last_error text,
    attach_sent_at timestamptz,
    idle_since timestamptz,
    disconnected_since timestamptz,
    cancel_sent_at timestamptz,
    -- Что из денежных пушей уже ушло (слоты повторов), чтобы не стучаться в очередь каждый тик.
    alert_state jsonb not null default '{}'::jsonb,
    constraint cloud_windows_name_check check (name = 'canterlot-runner-' || id::text)
);

-- Живое окно одно: вторая защита рядом с замком строки `cloud_budget`.
create unique index cloud_windows_one_live on cloud_windows ((true))
    where state in ('create_intent', 'create_unknown', 'identified', 'ready', 'draining', 'deleting');
create index cloud_windows_month on cloud_windows (month);
create index cloud_windows_created on cloud_windows (created_at desc);

-- Команда несущей: подключить облачный раннер.
alter table commands drop constraint if exists commands_kind_check;
alter table commands add constraint commands_kind_check check (kind in (
  'deliver_input','cancel_input','withdraw_input','interrupt','start_execution',
  'stop_execution','deliver_decision','compact_session','remove_worktree',
  'resolve_owner_lost','steer_input','revoke_secret','goal_command',
  'start_inplace_job','cancel_inplace_job',
  'stage_source','collect_outputs','cleanup_source','owner_job_finished',
  'attach_runner'
));

-- Денежные пуши владельцу без разговора.
alter table owner_pushes drop constraint owner_pushes_kind_check;
alter table owner_pushes add constraint owner_pushes_kind_check check (
  (kind in ('waiting', 'turn_finished', 'failure', 'routine_report') and conversation_id is not null)
  or (kind in ('sign_in', 'sign_in_failures', 'sign_in_lock', 'session_country',
               'credential_expiry', 'credential_auth', 'schedule_failed', 'cloud_alert')
      and conversation_id is null)
);
