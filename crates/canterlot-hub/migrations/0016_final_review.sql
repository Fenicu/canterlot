alter table conversations add column resume_pending_until timestamptz;
update conversations set resume_pending_until=clock_timestamp()+interval '14 days' where resume_pending;
create index conversations_resume_expiry on conversations(resume_pending_until) where resume_pending;
create index events_activity_repair on events(scope,committed_at desc)
  where kind not in ('block_delta','derived_changed','scheduled_changed');
