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
