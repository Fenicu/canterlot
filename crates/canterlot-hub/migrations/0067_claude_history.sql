-- Сессия продолжает готовую историю Claude Code: дата беседы для тихой строки ленты.
alter table conversations add column continued_from_at timestamptz;
