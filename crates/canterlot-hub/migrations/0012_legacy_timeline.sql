-- Старые вводы и блоки должны делить один порядок, иначе before пропускает совпавший ключ.
with legacy as (
  select conversation_id, id, 'input' as kind, created_at
  from inputs where created_seq < 0 or feed_seq < 0
  union all
  select conversation_id, block_id, 'block', created_at
  from blocks where created_seq < 0
), ordered as (
  select id, kind,
    row_number() over (partition by conversation_id order by created_at, id, kind)
      - count(*) over (partition by conversation_id) - 1 as seq
  from legacy
), moved_inputs as (
  update inputs i set
    created_seq = case when i.created_seq < 0 then o.seq else i.created_seq end,
    feed_seq = case when i.feed_seq < 0 then o.seq else i.feed_seq end
  from ordered o where o.kind = 'input' and o.id = i.id
)
update blocks b set created_seq = o.seq
from ordered o where o.kind = 'block' and o.id = b.block_id;
