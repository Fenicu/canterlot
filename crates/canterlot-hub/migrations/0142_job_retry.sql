-- Один повтор задачи, потерянной вместе с прерванной облачной машиной.
--
-- `retry_pending` — отдельное состояние, а не флаг: задача в нём не стоит ни в одном отборе
-- по состоянию (расстановка, очередь машины, сроки, потеря), поэтому не размещается и не
-- теряется, пока облачный код не подтвердит, что прежняя машина остановлена, и не выпустит её
-- (`release_retry_pending_in_tx`) или не закроет (`lose_retry_pending_in_tx`).
alter table jobs drop constraint jobs_state_check;
alter table jobs add constraint jobs_state_check check (state in (
    'waiting_runner', 'staging', 'queued', 'dispatched', 'waiting_lease', 'running',
    'collecting', 'retry_pending', 'succeeded', 'failed', 'timed_out', 'canceled', 'lost'));

-- Ждавшая названную машину задача остаётся без машины и в `retry_pending`.
alter table jobs drop constraint jobs_placed_check;
alter table jobs add constraint jobs_placed_check check (
    (machine_id is not null and workdir is not null)
    or state in ('waiting_runner', 'retry_pending', 'canceled', 'lost'));

alter table jobs
    -- Сколько раз задачу уже вернули в очередь после прерывания машины: повтор один.
    add column preemption_retries integer not null default 0
        check (preemption_retries between 0 and 1),
    -- Начало отсчёта срока ожидания раннера у возвращённой задачи; пусто — от подачи.
    add column placement_waiting_since timestamptz;

create index jobs_retry_pending on jobs (machine_id) where state = 'retry_pending';
