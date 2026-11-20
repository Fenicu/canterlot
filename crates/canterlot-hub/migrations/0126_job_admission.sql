-- Повтор вызова `job_run` по идентификатору вызова ищется внутри разговора: один и тот же
-- идентификатор у двух разговоров не должен сводить их к одной задаче.
drop index jobs_call;
create unique index jobs_call on jobs (conversation_id, call_id)
  where conversation_id is not null and call_id is not null;
