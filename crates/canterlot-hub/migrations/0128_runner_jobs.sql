-- Задачи на раннерах (этап B2): размещение хабом, подготовка исходников и сбор результатов
-- несущей машиной. Задача без машины ждёт в `waiting_runner`; с исходниками она проходит
-- `staging` до запуска и `collecting` после него.
alter table jobs
    alter column machine_id drop not null,
    alter column workdir drop not null;

alter table jobs drop constraint jobs_state_check;
alter table jobs add constraint jobs_state_check check (state in (
    'waiting_runner', 'staging', 'queued', 'dispatched', 'waiting_lease', 'running',
    'collecting', 'succeeded', 'failed', 'timed_out', 'canceled', 'lost'));

alter table jobs
    -- Метки, которые должны быть у раннера; `expected_minutes` — оценка длительности.
    add column labels text[] not null default '{}',
    add column expected_minutes integer not null default 30
        check (expected_minutes between 1 and 1440),
    -- Какие виды мест допустимы: `local` (сама несущая), `runner`, `cloud` (этап C).
    add column placement text[] not null default '{local,runner,cloud}',
    -- Исходники: абсолютный путь репозитория на машине заказчика и полный sha.
    add column source_repo text,
    add column source_rev text,
    add column source_domain text,
    -- Относительные пути результатов, которые несущая заберёт после запуска.
    add column outputs text[] not null default '{}',
    -- Машина, которая готовит исходники и собирает результаты (для `source` — подавшая).
    add column carrier_id uuid references machines(id),
    add column outputs_dir text,
    add column outputs_note text,
    -- Собранные файлы: `[{"path", "bytes"}]` из факта несущей.
    add column outputs_files jsonb,
    -- Подана терминалом владельца (`canterlot-worker job run`), без разговора.
    add column owner_job boolean not null default false,
    -- Итог раннера, пока задача в `collecting`: настоящее терминальное состояние наступает
    -- после сбора и очистки.
    add column outcome_state text,
    add column outcome_reason text,
    add column collecting_at timestamptz,
    -- Очистка исходников: когда подтверждена и сколько раз команда выдавалась.
    add column cleanup_attempts integer not null default 0,
    add column cleanup_sent_at timestamptz,
    add column cleaned_at timestamptz,
    add constraint jobs_source_shape_check check (
        (source_repo is null) = (source_rev is null)
        and (source_repo is null) = (source_domain is null)
        and (source_repo is null or carrier_id is not null)),
    -- Машина и каталог нужны, как только задача покинула ожидание раннера; отменённая из
    -- ожидания остаётся без них.
    add constraint jobs_placed_check check (
        (machine_id is not null and workdir is not null)
        or state in ('waiting_runner', 'canceled'));

create index jobs_waiting_runner on jobs (carrier_id, created_at, id)
    where state = 'waiting_runner';
create index jobs_collecting on jobs (collecting_at) where state = 'collecting';
create index jobs_uncleaned on jobs (cleanup_sent_at)
    where source_repo is not null and cleaned_at is null
      and state in ('succeeded', 'failed', 'timed_out', 'canceled', 'lost');

-- Команды несущей машине: подготовка, сбор и очистка, итог задачи владельца.
alter table commands drop constraint if exists commands_kind_check;
alter table commands add constraint commands_kind_check check (kind in (
  'deliver_input','cancel_input','withdraw_input','interrupt','start_execution',
  'stop_execution','deliver_decision','compact_session','remove_worktree',
  'resolve_owner_lost','steer_input','revoke_secret','goal_command',
  'start_inplace_job','cancel_inplace_job',
  'stage_source','collect_outputs','cleanup_source','owner_job_finished'
));
