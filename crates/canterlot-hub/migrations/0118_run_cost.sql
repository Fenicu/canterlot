-- Цена хода по модели. Пусто — цена неизвестна, а не ноль: ноль с источником cli —
-- бесплатная модель. Источник: cli (назвал сам CLI) или prices (таблица цен хаба).
-- Цена пишется при конце хода и задним числом не пересчитывается.
alter table run_model_usage
  add column cost_micro_usd bigint check (cost_micro_usd >= 0),
  add column cost_source text check (cost_source in ('cli', 'prices')),
  add constraint run_model_usage_cost_source_matches
    check ((cost_micro_usd is null) = (cost_source is null));

-- Свёртка удалённой делегации хранит цену внутри `models`, а число ходов без цены —
-- отдельно. Свёрнутое до этой миграции цен не имело: все такие ходы без цены.
alter table conversation_usage_rollup
  add column unknown_cost_turns bigint not null default 0 check (unknown_cost_turns >= 0);
update conversation_usage_rollup set unknown_cost_turns = turns;
