-- Архивация завершённых задач доски плана: строки остаются для истории, но не считаются
-- в лимите и не попадают в обычный список. Номер задачи по-прежнему идёт по счётчику
-- plan_boards.next_number и после архивации не переиспользуется.

alter table plan_tasks add column archived_at timestamptz;

-- Уборка старых завершённых задач и поиск самых старых из них при нехватке места.
create index plan_tasks_finished on plan_tasks (orchestrator_conversation_id, updated_at, number)
    where archived_at is null and state in ('done', 'cancelled');
