alter table conversations
  add column title_source text not null default 'auto' check (title_source in ('auto','user')),
  add column color text check (color is null or color ~ '^[a-z0-9_-]{1,24}$'),
  add column pinned boolean not null default false, add column pinned_at timestamptz, add column archived_at timestamptz,
  add column model text not null default '', add column effort text not null default '', add column permission_mode text not null default '',
  add column instruction_profile text not null default 'owner',
  add column pending_settings jsonb not null default '{}', add column settings_rev bigint not null default 0,
  add column crunch_enabled boolean not null default false, add column crunch_resume_text text,
  add column reserve_override_until timestamptz, add column updated_at timestamptz not null default clock_timestamp();
alter table inputs add column if not exists retry_of uuid references inputs(id);
alter table conversation_derived add column effective_model text not null default '', add column effective_effort text not null default '',
  add column effective_permission_mode text not null default '', add column crunch_enabled boolean not null default false;
update conversation_derived d set effective_model=c.model, effective_effort=c.effort,
  effective_permission_mode=c.permission_mode, crunch_enabled=c.crunch_enabled from conversations c where c.id=d.conversation_id;
create table conversation_groups (id uuid primary key, owner_id uuid not null references owners(id), name text not null check (length(name) between 1 and 60),
  position integer not null default 0, created_at timestamptz not null default clock_timestamp());
create unique index conversation_groups_name on conversation_groups (owner_id, lower(name collate "und-x-icu"));
create table conversation_group_members (group_id uuid not null references conversation_groups(id) on delete cascade,
  conversation_id uuid not null references conversations(id) on delete cascade, primary key (group_id, conversation_id));
create index conversation_group_members_conversation on conversation_group_members (conversation_id);
create index conversations_owner_list on conversations (owner_id, archived_at, pinned desc, pinned_at desc nulls last, last_activity_at desc nulls last, id);
