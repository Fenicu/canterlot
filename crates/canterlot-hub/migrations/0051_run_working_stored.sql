-- Рабочее время хода считается один раз, когда ход закрыт; представление считает на лету
-- только открытые ходы (coalesce не вычисляет правый аргумент при заданном левом).
alter table runs add column working_ms bigint;
update runs r set working_ms = w.working_ms
  from run_working w
  where w.input_command_id = r.input_command_id and r.finished_at is not null;
create or replace view run_working as
select r.input_command_id,
       r.conversation_id,
       coalesce(r.working_ms, greatest(0,
         coalesce(r.duration_ms,
           (extract(epoch from (coalesce(r.finished_at, now()) - r.started_at)) * 1000)::bigint)
         - coalesce((
             select sum(extract(epoch from (upper(merged) - lower(merged))) * 1000)::bigint
             from (
               select unnest(coalesce(range_agg(tstzrange(wait.started, wait.ended, '[)')), '{}'::tstzmultirange)) as merged
               from (
                 select greatest(q.created_at, r.started_at) as started,
                        least(case when q.state = 'open' then now() else q.updated_at end,
                              coalesce(r.finished_at, now())) as ended
                 from requests q
                 where q.execution_id = r.execution_id
                   and q.kind in ('permission', 'question')
                   and q.created_at < coalesce(r.finished_at, now())
                   and (case when q.state = 'open' then now() else q.updated_at end) > r.started_at
               ) wait
               where wait.started < wait.ended
             ) merged
         ), 0)
       )) as working_ms
from runs r;
