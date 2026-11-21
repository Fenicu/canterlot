-- Лимиты по учётным записям: у каждого харнесса машины есть метка `account`
-- (worker.toml), и состояние лимитов, удержания, опрос и учёт ходов идут по паре
-- (провайдер, учётная запись). Все существующие строки становятся `owner`: отчитывалась
-- одна машина, смешанных данных нет.

alter table machine_harnesses add column account text not null default 'owner'
  check (account ~ '^[a-z0-9-]{1,40}$');

alter table provider_limits add column account text not null default 'owner';
alter table provider_limits drop constraint provider_limits_pkey;
alter table provider_limits add primary key (provider, account);

alter table limit_snapshots add column account text not null default 'owner';
drop index limit_snapshots_lookup;
create index limit_snapshots_lookup
  on limit_snapshots (provider, account, window_kind, observed_at desc);

alter table limit_polls add column account text not null default 'owner';
alter table limit_polls drop constraint limit_polls_pkey;
alter table limit_polls add primary key (provider, account);

-- Учётная запись хода фиксируется при его начале: недельные снимки до и после берутся
-- из её строки, даже если метку харнесса потом сменят.
alter table runs add column account text not null default 'owner';

-- Задания `limit_check` и `limit_rule` уникальны по паре. Полезная нагрузка `owner`
-- остаётся прежней, без ключа `account`: ожидающие задания пережили обновление нетронутыми.
drop index scheduled_jobs_limit_check_once;
drop index scheduled_jobs_limit_rule_once;
create unique index scheduled_jobs_limit_check_once
  on scheduled_jobs ((payload->>'provider'), (coalesce(payload->>'account', 'owner')))
  where kind = 'limit_check' and state = 'pending';
create unique index scheduled_jobs_limit_rule_once
  on scheduled_jobs ((payload->>'provider'), (coalesce(payload->>'account', 'owner')))
  where kind = 'limit_rule' and state = 'pending';
