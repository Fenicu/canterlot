-- Сводка устаревших задач плана для оркестратора: когда была последняя и какие задачи
-- в ней уже назывались (ID задачи → «причина:ревизия»), чтобы не повторять неизменное.
create table plan_reminders (
  orchestrator_conversation_id uuid primary key references conversations(id) on delete cascade,
  last_digest_at timestamptz,
  noted jsonb not null default '{}'::jsonb check (jsonb_typeof(noted) = 'object')
);
