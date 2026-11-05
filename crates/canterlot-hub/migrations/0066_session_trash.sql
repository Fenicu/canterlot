-- Корзина владельца: ручное удаление прячет сессию на срок хранения вместо
-- окончательной уборки. Пока отметка стоит, сессия не видна в списках, поиске,
-- счётчиках и инструментах агента, но её строки, рабочая копия и вложения целы.
alter table conversations add column trashed_at timestamptz;
-- Уборка идёт по сроку и пачками: частичный индекс не содержит живых сессий.
create index conversations_trash_retention on conversations (trashed_at)
    where trashed_at is not null;
