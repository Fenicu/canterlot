-- Повтор вызова `job_run` по идентификатору вызова ищется внутри разговора: один и тот же
-- идентификатор у двух разговоров не должен сводить их к одной задаче.
drop index jobs_call;
create unique index jobs_call on jobs (conversation_id, call_id)
  where conversation_id is not null and call_id is not null;

-- Ждущий вызов `job_run`: экземпляр хаба, в котором он живёт, и срок его ожидания. Перевод
-- доставки на сообщение при старте хаба и по таймеру касается только задач, чей ждущий
-- принадлежит другому экземпляру или чей срок вышел; срок отсчитывается от фиксации заявки.
alter table jobs
  add column waiter_instance uuid,
  add column waiter_deadline timestamptz;

-- Поколение допуска удалённой работы: растёт каждый раз, когда выключатель
-- `accepts_remote_work` переводится из «вкл» в «выкл». Ожидающие удалённые запуски (элементы
-- очереди, карточки) и ждущие в очереди удалённые задачи запоминают поколение при заявке;
-- запуск и диспетчеризация отказывают, если поколение машины выросло: работа, ждавшая в момент
-- выключения, не оживает от повторного включения. Приращение делает триггер, поэтому его не
-- обойти ни одним путём записи.
alter table machines
  add column remote_admission_generation bigint not null default 0;
alter table jobs add column admission_generation bigint;

create function machines_bump_admission_generation() returns trigger
language plpgsql as $$
begin
  if old.accepts_remote_work and not new.accepts_remote_work then
    new.remote_admission_generation := old.remote_admission_generation + 1;
  end if;
  return new;
end
$$;

create trigger machines_bump_admission_generation
  before update of accepts_remote_work on machines
  for each row execute function machines_bump_admission_generation();

-- Снимок «закончена» из отчёта при подключении не несёт хвоста вывода: задача ждёт факта
-- `JobFinished` из журнала воркера. Снимок запоминается, чтобы задача не повисла, если факт
-- так и не придёт (служба закрывает её по снимку без хвоста).
alter table jobs
  add column finish_report jsonb,
  add column finish_reported_at timestamptz;
