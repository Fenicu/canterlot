-- Бюджет облака, автоматический режим и карточки предложений (этап C2b): настройки
-- владельца живут в единственной строке `cloud_budget`, а решение по окну — в обычной
-- карточке `requests.kind = 'cloud_window'` рядом с предложениями настроек.

alter table cloud_budget
    add column auto_mode boolean not null default false,
    -- Растёт при каждом изменении настроек владельцем: защищает от потери правки,
    -- сделанной пока карточка была открыта.
    add column revision bigint not null default 0 check (revision >= 0),
    -- Месяц (`YYYY-MM`, Европа/Москва), в котором последний раз ушло оповещение о пороге.
    add column alert_80_month text,
    add column alert_100_month text;

alter table cloud_budget
    add constraint cloud_budget_alert_80_check
    check (alert_80_month is null or alert_80_month ~ '^[0-9]{4}-(0[1-9]|1[0-2])$'),
    add constraint cloud_budget_alert_100_check
    check (alert_100_month is null or alert_100_month ~ '^[0-9]{4}-(0[1-9]|1[0-2])$');

-- Предложение запустить облачное окно решается владельцем так же, как предложение настройки.
alter table requests drop constraint requests_kind_check;
alter table requests add constraint requests_kind_check
  check (kind in ('permission', 'question', 'spawn_approval', 'settings_change', 'cloud_window'));

-- Предложение создаёт не более одного окна: вторая защита рядом с замком строки `cloud_budget`
-- и проверкой состояния карточки под её строковым замком.
create unique index cloud_windows_one_proposal on cloud_windows (proposal_id)
    where proposal_id is not null;

-- Непрерывный период недоступности/паузы статичного раннера для автоматического режима:
-- одна свежая доступность сбрасывает отметку, десять минут подряд — основание для старта.
create table cloud_auto_unavailable (
    machine_id uuid primary key references machines(id) on delete cascade,
    since timestamptz not null
);

-- Эпизод ожидающей облачной работы: карточка хаба выдаётся один раз, пока очередь не
-- опустеет. После опустошения признак сбрасывается, и новое ожидание создаёт новый эпизод.
create table cloud_auto_episode (
    singleton boolean primary key default true check (singleton),
    active boolean not null default false,
    card_id uuid references requests(id)
);
insert into cloud_auto_episode (singleton) values (true);
