-- Признак приёма результата: сессия закрыта родителем через session_accept.
-- Отдельно от archived_at: архив ставит и владелец, а срок хранения отсчитывается
-- только от приёма. NULL — сессия не принята и сроком хранения не затронута.
alter table conversations add column accepted_at timestamptz;
create index conversations_accepted_retention on conversations (accepted_at)
    where accepted_at is not null;
