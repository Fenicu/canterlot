-- Рутины: отчёт по расписанию из контекста существующей сессии.
-- Копия-читатель — та же скрытая боковая копия (`side_of`), но другого рода:
-- у родителя может быть один интерактивный чат владельца и сколько угодно
-- временных копий отчётов. Уникальность `side_of` остаётся только у чата.
alter table conversations
  add column side_kind text not null default 'chat' check (side_kind in ('chat', 'report'));
drop index conversations_side_of;
create unique index conversations_side_of on conversations(side_of)
  where side_of is not null and side_kind = 'chat';

-- Карточка отчёта в ленте родителя. Это не ввод и не блок ленты: у неё нет места в
-- журнале, очереди и счётчиках, поэтому агент никогда её не получает, а родитель
-- не оживает, не просыпается и не получает толчков.
create table routine_reports (
  id uuid primary key,
  owner_id uuid not null references owners(id),
  conversation_id uuid not null references conversations(id) on delete cascade,
  schedule_id uuid not null references schedules(id) on delete cascade,
  -- Строка запуска может уйти уборкой журнала раньше карточки: карточка не зависит от неё.
  run_id uuid references schedule_runs(id) on delete set null,
  -- Копия-читатель. Пока она не удалена, поле заполнено; ссылки нет: строку копии
  -- удаляет приложение, а не каскад.
  copy_id uuid,
  title text not null,
  state text not null check (state in ('pending', 'ready', 'failed')),
  body text,
  error text,
  -- Момент, когда ход копии начался: от него идёт предел в 60 минут.
  turn_started_at timestamptz,
  created_at timestamptz not null default clock_timestamp(),
  updated_at timestamptz not null default clock_timestamp(),
  completed_at timestamptz,
  dismissed_at timestamptz,
  dismiss_reason text check (dismiss_reason in ('owner', 'replaced')),
  check ((dismissed_at is null) = (dismiss_reason is null))
);
-- Не больше одной открытой карточки расписания на родителя.
create unique index routine_reports_open on routine_reports(schedule_id, conversation_id)
  where dismissed_at is null;
create index routine_reports_pending on routine_reports(created_at) where state = 'pending';
create index routine_reports_copy on routine_reports(copy_id) where copy_id is not null;
create index routine_reports_conversation on routine_reports(conversation_id)
  where dismissed_at is null;

-- Пуш «отчёт готов» привязан к родителю, как пуш о ходе.
alter table owner_pushes drop constraint owner_pushes_kind_check;
alter table owner_pushes add constraint owner_pushes_kind_check check (
  (kind in ('waiting', 'turn_finished', 'failure', 'routine_report') and conversation_id is not null)
  or (kind in ('sign_in', 'sign_in_failures', 'sign_in_lock', 'session_country',
               'credential_expiry', 'credential_auth', 'schedule_failed')
      and conversation_id is null)
);
