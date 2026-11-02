-- Начало текущего хода: с него считается молчание для «Подвисла?», а не с последнего
-- события прошлого хода. Меняется вместе с идентичностью run_slot.
alter table queue_state add column run_slot_since timestamptz not null default clock_timestamp();
