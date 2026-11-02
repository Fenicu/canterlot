-- Рабочее время хода: его длительность минус ожидание владельца внутри хода.
-- Ожидание — это открытый запрос разрешения или вопроса; его конец берётся из updated_at,
-- а у ещё открытого запроса — текущий момент. Интервал обрезается границами хода, чтобы
-- запрос, переживший ход, не вычитался дважды.
create view run_working as
select r.input_command_id,
       r.conversation_id,
       greatest(0,
         coalesce(r.duration_ms,
           (extract(epoch from (coalesce(r.finished_at, now()) - r.started_at)) * 1000)::bigint)
         - coalesce((
             select sum(greatest(0,
               (extract(epoch from (
                 least(case when q.state = 'open' then now() else q.updated_at end,
                       coalesce(r.finished_at, now()))
                 - greatest(q.created_at, r.started_at)
               )) * 1000)::bigint))::bigint
             from requests q
             where q.execution_id = r.execution_id
               and q.kind in ('permission', 'question')
               and q.created_at < coalesce(r.finished_at, now())
               and (case when q.state = 'open' then now() else q.updated_at end) > r.started_at
           ), 0)
       ) as working_ms
from runs r;
