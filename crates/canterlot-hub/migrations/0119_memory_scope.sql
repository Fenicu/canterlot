-- Домен и проект общей памяти у каждого разговора. NULL в домене — памяти нет.
-- Поля ставятся один раз при создании строки и больше не пересчитываются; единственное
-- исключение — проверка запуска, которая только обнуляет домен.
alter table conversations
  add column memory_domain text check (memory_domain in ('work', 'personal')),
  add column memory_project text;

-- Канонические корни работы, о которых сообщил воркер с возможностью memory_scope_v1.
-- NULL — машина ещё не сообщала, и классифицировать нечем.
alter table machines add column memory_work_roots jsonb;

-- Осталась ли проверка запуска недовольна: домен обнулён, потому что настоящий путь
-- рабочего каталога противоречит сохранённому домену. Видно в сведениях о сессии.
alter table conversations add column memory_domain_conflict_at timestamptz;

-- Заполнение существующих строк. Корни берут домен по сохранённому рабочему каталогу
-- и правилу по умолчанию (`/home/<user>/work`), остальные каталоги считаются личными.
-- Рабочая копия классифицируется по каталогу исходного репозитория; корень внутри базы
-- рабочих копий без указания источника нейтрален и остаётся без памяти.
with recursive classified as (
  select c.id,
         coalesce(c.worktree ->> 'source_workdir', c.workdir) as project,
         case
           when m.worktree_root is not null and c.worktree is null
                and (c.workdir = m.worktree_root or c.workdir like m.worktree_root || '/%')
             then null
           when coalesce(c.worktree ->> 'source_workdir', c.workdir) ~ '^/home/[^/]+/work(/|$)'
             then 'work'
           else 'personal'
         end as domain
    from conversations c
    join machines m on m.id = c.machine_id
   where c.parent_conversation_id is null
     and c.side_of is null
     and c.spawn_depth = 0
),
-- Дети идут по `parent_conversation_id` и `side_of` от корня вниз. Ссылка, которой нет
-- (родитель не найден или цепочка оборвана), остаётся без домена.
chain as (
  select id, domain, project from classified
  union all
  select c.id, chain.domain, chain.project
    from conversations c
    join chain on chain.id = coalesce(c.side_of, c.parent_conversation_id)
)
update conversations c
   set memory_domain = chain.domain,
       memory_project = case when chain.domain is null then null else chain.project end
  from chain
 where c.id = chain.id
   and chain.domain is not null;

-- Связи между разговорами, созданные до появления доменов: после заполнения у части из них
-- концы оказались в разных доменах или без домена. Агентам такие связи больше не положены
-- (разговор без домена не равен даже другому такому же), поэтому открытые подписки и
-- взведённые будильники на сессии с несовпавшими концами удаляются. Закрытые подписки
-- и сработавшие будильники ничего не доставят, их не трогаем.
delete from session_subscriptions s
 using conversations subscriber, conversations target
 where subscriber.id = s.subscriber_id
   and target.id = s.target_id
   and s.closed_at is null
   and (subscriber.memory_domain is null
        or subscriber.memory_domain is distinct from target.memory_domain);

delete from alarms a
 using conversations owner_conversation
 where owner_conversation.id = a.conversation_id
   and a.state = 'armed'
   and a.condition ->> 'kind' = 'session_state'
   and exists (
     select 1
       from jsonb_array_elements_text(a.condition -> 'session_ids') named(id)
       join conversations target on target.id = named.id::uuid
      where owner_conversation.memory_domain is null
         or target.memory_domain is distinct from owner_conversation.memory_domain);

-- Ждущие доставки вводы от другой сессии, у которой с получателем не совпали домены (или
-- у одной из сторон домена нет): без этого первая же выдача после обновления передала бы
-- их через границу. Ввод отменяется так же, как отмена владельцем: состояние `cancelled`
-- и завершённая удержанная выдача. Вводы владельца (без отправителя-разговора) не трогаются.
with crossing as (
  update inputs i
     set state = 'cancelled', cancel_requested = false
    from conversations sender, conversations target
   where i.state = 'queued'
     and i.origin is not null
     and i.origin ->> 'from' ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
     and sender.id = (i.origin ->> 'from')::uuid
     and target.id = i.conversation_id
     and (sender.memory_domain is null
          or sender.memory_domain is distinct from target.memory_domain)
  returning i.command_id
)
update commands c
   set delivery_state = 'completed', completed_at = coalesce(c.completed_at, now()),
       last_status = 'cancel_won', last_status_rank = 3,
       result = '{"cancelled": true}'::jsonb
  from crossing
 where c.id = crossing.command_id
   and c.kind = 'deliver_input' and c.delivery_state = 'held';
