-- Конкретная модель, на которой CLI выполнил ход: псевдоним (`fable[1m]`) CLI
-- разрешает сам, и соответствие меняется с выходом новой версии.
-- Исполнение хранит пару «что просили — что получили» по первому блоку основного агента.
alter table executions add column model_requested text, add column model_resolved text;
-- Опубликованная проекция для списка и шапки; пусто — разрешение ещё неизвестно.
alter table conversation_derived add column effective_model_resolved text not null default '';
-- Последнее наблюдённое разрешение псевдонима на машине и прежнее значение при смене.
create table model_resolutions (
  machine_id uuid not null references machines(id) on delete cascade,
  harness text not null,
  alias text not null,
  resolved text not null,
  observed_at timestamptz not null,
  previous text,
  changed_at timestamptz,
  primary key (machine_id, harness, alias));
