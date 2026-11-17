-- Подарки провайдера: сбросы лимитов Codex. Строка на машину и провайдера, как у конца
-- входа: учётная запись своя на каждой машине, а показ берёт самую свежую строку среди
-- машин владельца. Показ, а не трата: хаб ничего не покупает и не расходует.
create table provider_credits (
  machine_id uuid not null references machines(id) on delete cascade,
  provider text not null,
  -- Сколько сбросов доступно; null — провайдер их ещё не называл.
  reset_count int,
  -- Доступные сбросы: [{"title","expires_at_ms","granted_at_ms"}].
  resets jsonb not null default '[]',
  observed_at timestamptz not null default clock_timestamp(),
  primary key (machine_id, provider)
);
