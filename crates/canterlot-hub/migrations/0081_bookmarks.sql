-- Избранные сообщения разговора. Цель задаётся парой (вид, идентификатор) и проверяется
-- при добавлении; текст не дублируется, чтобы правка ввода и сжатие истории не оставляли
-- устаревший фрагмент. Удаление разговора убирает закладки каскадом.
create table bookmarks (
  id uuid primary key,
  owner_id uuid not null references owners(id),
  conversation_id uuid not null references conversations(id) on delete cascade,
  target_kind text not null check (target_kind in ('input','block')),
  target_id uuid not null,
  created_at timestamptz not null default clock_timestamp(),
  unique (conversation_id, target_kind, target_id)
);
create index bookmarks_owner_created on bookmarks (owner_id, created_at desc);
