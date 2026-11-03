alter table provider_limits add column data_confidence text not null default 'exact'
  check (data_confidence in ('exact','estimated'));
