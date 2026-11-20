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

-- Снимок «закончена» из отчёта при подключении не несёт хвоста вывода: задача ждёт факта
-- `JobFinished` из журнала воркера. Снимок запоминается, чтобы задача не повисла, если факт
-- так и не придёт (служба закрывает её по снимку без хвоста).
alter table jobs
  add column finish_report jsonb,
  add column finish_reported_at timestamptz;
