alter table scheduled_jobs add column failures integer not null default 0 check (failures >= 0);
update scheduled_jobs set failures=case when kind='message' then greatest(attempts,1) else 1 end
  where result->>'error' is not null;
-- Возвращаем единственный путь снятия удержания после временного сбоя.
with recoverable as (
  select j.id,row_number() over(partition by j.kind,
    case when j.kind='wake' then j.conversation_id end,
    case when j.kind='wake' then j.payload->>'hold_key' else j.payload->>'provider' end
    order by j.updated_at desc,j.id desc) n
  from scheduled_jobs j where j.kind in ('wake','limit_check') and j.state='failed'
    and j.result->>'error' in ('job_error','database_error')
    and not exists(select 1 from scheduled_jobs p where p.state='pending' and p.kind=j.kind and (
      (j.kind='wake' and p.conversation_id=j.conversation_id
        and p.payload->>'hold_key' is not distinct from j.payload->>'hold_key')
      or (j.kind='limit_check' and p.payload->>'provider' is not distinct from j.payload->>'provider')
    ))
)
update scheduled_jobs j set state='pending',attempts=greatest(j.attempts-1,0),
  run_at=least(j.run_at,clock_timestamp()+interval '60 seconds'),updated_at=clock_timestamp()
  from recoverable r where j.id=r.id and r.n=1;

alter table inputs add column client_idempotency_key text, add column submission_hash bytea;
-- Для старых дублей ключ остаётся у первого ввода.
with first_inputs as (
  select id,row_number() over(partition by owner_id,idempotency_key order by committed_at,id) n
  from inputs
)
update inputs i set client_idempotency_key=i.idempotency_key from first_inputs f where i.id=f.id and f.n=1;
update inputs i set submission_hash=k.request_hash from idempotency_keys k
  where k.owner_id=i.owner_id and k.key=i.client_idempotency_key
    and k.operation='submit_input' and k.response->>'input_id'=i.id::text;
create unique index inputs_client_idempotency on inputs(owner_id,client_idempotency_key);

alter table executions add column owner_stop_input_seq bigint,
  add column owner_stop_requested boolean not null default false;
update executions e set owner_stop_requested=true
  where exists(select 1 from commands c where c.execution_id=e.id and c.kind='stop_execution');
update executions e set owner_stop_input_seq=coalesce((
  select max(i.input_seq) from inputs i join commands c on c.id=i.command_id
  where c.execution_id=e.id and c.created_at <= (
    select max(s.created_at) from commands s where s.execution_id=e.id and s.kind='interrupt'
  )
),0) where exists(select 1 from commands s where s.execution_id=e.id and s.kind='interrupt');
