alter table requests add column created_seq bigint;

update requests r set created_seq = e.scope_seq
  from events e
 where e.kind = 'request_opened'
   and e.conversation_id = r.conversation_id
   and e.payload ->> 'request_id' = r.id::text;

create index requests_feed on requests(conversation_id, created_seq) where created_seq is not null;
