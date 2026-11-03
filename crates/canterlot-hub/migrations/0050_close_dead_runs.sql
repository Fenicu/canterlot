-- Ход, закрытый хабом при конце исполнения: CLI умер, конца хода не прислал.
-- Поздний конец хода, если всё же придёт, заменяет оценку хаба.
alter table runs add column forced_close boolean not null default false;
-- Ходы уже закончившихся исполнений закрываются временем конечного перехода.
update runs r set finished_at = greatest(r.started_at, e.updated_at),
  duration_ms = (extract(epoch from (greatest(r.started_at, e.updated_at) - r.started_at)) * 1000)::bigint,
  exact = false, forced_close = true
  from executions e
  where e.id = r.execution_id and r.finished_at is null and e.state in ('stopped', 'failed');
-- Пересечения считаются заново одним проходом по ходам провайдера в порядке начала:
-- ход пересекается с ранним, если какой-то ранний кончается позже его начала, и с поздним,
-- если следующий начинается раньше его конца.
with ordered as (
  select input_command_id,
         max(coalesce(finished_at, 'infinity')) over (partition by provider order by started_at, input_command_id
           rows between unbounded preceding and 1 preceding) > started_at as after_earlier,
         lead(started_at) over (partition by provider order by started_at, input_command_id)
           < coalesce(finished_at, 'infinity') as before_later
  from runs where provider is not null
)
update runs r set overlapped = coalesce(o.after_earlier, false) or coalesce(o.before_later, false)
  from ordered o
  where o.input_command_id = r.input_command_id
    and r.overlapped is distinct from (coalesce(o.after_earlier, false) or coalesce(o.before_later, false));
