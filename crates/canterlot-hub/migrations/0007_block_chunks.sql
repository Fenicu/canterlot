create table block_chunks (
  block_id uuid not null references blocks(block_id) on delete cascade,
  seq bigint not null,
  text text not null,
  primary key (block_id, seq)
);
alter table blocks add column revision_gap boolean not null default false;
update blocks b set revision_gap = true from events e
  where e.kind = 'block_revision_gap' and e.payload->>'block_id' = b.block_id::text;
insert into block_chunks (block_id, seq, text)
  select block_id, rev, content from blocks where state = 'streaming' and content <> '';
update blocks set content = '' where state = 'streaming';
