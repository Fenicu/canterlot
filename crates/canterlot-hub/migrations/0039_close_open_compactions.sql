-- Терминальное исполнение не оставляет открытых сжатий: строка не должна идти вечно.
update compactions c
set ok = false, finished_at = coalesce(c.finished_at, now())
where c.ok is null
  and exists (
    select 1 from executions e
    where e.id = c.execution_id and e.state in ('stopped', 'failed')
  );
