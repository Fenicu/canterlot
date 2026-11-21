-- Исправления по ревью этапа B: названная машина у ждущей задачи и «с какого времени раннер
-- недоступен» для правила этапа C.

-- Задача, названная на конкретную машину, у которой нет свободного места, ждёт в
-- `waiting_runner` именно её: расстановка берёт только эту машину, отзыв машины закрывает
-- ждущих.
alter table jobs
    add column requested_machine_id uuid references machines(id);
create index jobs_waiting_requested on jobs (requested_machine_id)
    where state = 'waiting_runner' and requested_machine_id is not null;

-- Ждущая задача может кончиться `lost` без машины (отзыв несущей или названной машины).
alter table jobs drop constraint jobs_placed_check;
alter table jobs add constraint jobs_placed_check check (
    (machine_id is not null and workdir is not null)
    or state in ('waiting_runner', 'canceled', 'lost'));

-- Время хаба, с которого доступность раннера стала `offline`; пусто при любом другом
-- состоянии. Правило «десять минут недоступности» этапа C отсчитывается от него.
alter table machines
    add column runner_offline_since timestamptz;
