-- Убрать оставшиеся следы Antigravity: поддержка харнесса снята полностью.

delete from provider_limits where provider = 'antigravity';
delete from limit_snapshots where provider = 'antigravity';
delete from limit_polls where provider = 'antigravity';
delete from provider_status where provider = 'antigravity';
delete from provider_credentials where provider = 'antigravity';
delete from runs where provider = 'antigravity';
delete from machine_harnesses where harness = 'antigravity' or provider = 'antigravity';
delete from model_resolutions where harness = 'antigravity';

-- Политика маршрутизации хранит имена харнессов внутри jsonb.
update owners
set routing_policy = jsonb_set(
  jsonb_set(routing_policy, '{providers}',
    coalesce(routing_policy->'providers', '{}'::jsonb) - 'antigravity'),
  '{task_types}',
  coalesce((
    select jsonb_object_agg(task.key, jsonb_set(task.value, '{candidates}',
      coalesce((
        select jsonb_agg(candidate) from jsonb_array_elements(task.value->'candidates') as candidate
        where candidate->>'harness' <> 'antigravity'
      ), '[]'::jsonb)))
    from jsonb_each(routing_policy->'task_types') as task
  ), '{}'::jsonb)
)
where coalesce(routing_policy->'providers', '{}'::jsonb) ? 'antigravity'
   or exists (
     select 1 from jsonb_each(routing_policy->'task_types') as task,
       jsonb_array_elements(task.value->'candidates') as candidate
     where candidate->>'harness' = 'antigravity'
   );
