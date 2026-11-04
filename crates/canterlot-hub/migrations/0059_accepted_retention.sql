-- Признак приёма результата: сессия закрыта родителем через session_accept.
-- Отдельно от archived_at: архив ставит и владелец, а срок хранения отсчитывается
-- только от приёма. NULL — сессия не принята и сроком хранения не затронута.
alter table conversations add column accepted_at timestamptz;
create index conversations_accepted_retention on conversations (accepted_at)
    where accepted_at is not null;

-- Сессии, принятые до появления колонки, получают срок из того, что приём уже оставил
-- в базе: закрытой подписки с причиной accepted, отработавшего задания уборки или
-- убранной рабочей копии. Без такого следа сессия не принята и остаётся как есть;
-- глубина и архив отсекают корневые и незакрытые сессии. Один UPDATE не держит
-- живую базу дольше своей транзакции.
update conversations c
set accepted_at = greatest(
    c.archived_at,
    coalesce(
        (select max(s.closed_at) from session_subscriptions s
         where s.target_id = c.id and s.closed_reason = 'accepted'
           and s.closed_at is not null),
        c.archived_at
    ),
    coalesce(
        (select max(coalesce(j.fired_at, j.updated_at)) from scheduled_jobs j
         where j.conversation_id = c.id and j.kind = 'session_task'
           and j.payload->>'action' = 'cleanup' and j.state = 'fired'),
        c.archived_at
    )
)
where c.accepted_at is null
  and c.spawn_depth > 0
  and c.archived_at is not null
  and (
      exists (
          select 1 from session_subscriptions s
          where s.target_id = c.id and s.closed_reason = 'accepted'
            and s.closed_at is not null
      )
      or exists (
          select 1 from scheduled_jobs j
          where j.conversation_id = c.id and j.kind = 'session_task'
            and j.payload->>'action' = 'cleanup' and j.state = 'fired'
      )
      or c.worktree->>'state' = 'removed'
  );
