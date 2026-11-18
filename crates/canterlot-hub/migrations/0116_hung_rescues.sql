-- Спасение хода, зависшего в генерации. Одна строка на разговор: имя хода, отметка
-- прогресса потока, эпизод зависания, ожидающее подтверждения воркера действие,
-- отложенная заметка родителю и окно наблюдения за выборками CPU.
create table hung_rescues (
  conversation_id uuid primary key references conversations(id) on delete cascade,
  execution_id uuid,
  -- command_id ввода, который идёт в слоте `running`.
  turn_id uuid,
  -- Прогресс потока на момент открытия эпизода: эпизод закрывается только более
  -- новым прогрессом или концом хода, а не любым провалом условия verdict.
  progress_mark timestamptz,
  -- Отметка прогресса журнала сессии (max committed_seq) на момент открытия эпизода:
  -- кнопки карточки отказывают, если журнал ушёл дальше.
  progress_checkpoint bigint,
  -- Ввод очереди, на который смотрит кнопка «Влить» карточки.
  card_input_id uuid,
  episode_no integer not null default 0,
  episode_id uuid,
  episode_active boolean not null default false,
  -- Ход уже предупредил владельца или получил действие: второе зависание того же хода
  -- получает только пуш. Сам по себе ввода не создаёт ни для какой сессии.
  alerted boolean not null default false,
  -- Автоматическое спасение подтверждено квитанцией воркера. Только после него
  -- родитель получает заметку; режим `alert` этот признак не ставит никогда.
  rescued boolean not null default false,
  second_notified boolean not null default false,
  -- Владелец или гонка закрыли карточку эпизода.
  resolved text,
  -- Прерывание или вливание отправлено и ждёт квитанции воркера.
  pending_kind text check (pending_kind in ('interrupt','steer')),
  pending_command_id uuid,
  pending_action text,
  pending_since timestamptz,
  last_outcome text,
  -- Заметка родителю: уходит после фиксации ребёнка, без ожидания блокировки родителя.
  parent_note jsonb,
  obs_execution_id uuid,
  obs_since timestamptz,
  obs_last_at timestamptz,
  obs_samples integer not null default 0,
  updated_at timestamptz not null default clock_timestamp()
);
create index hung_rescues_parent_note on hung_rescues(conversation_id) where parent_note is not null;
create index hung_rescues_pending on hung_rescues(pending_command_id) where pending_command_id is not null;
