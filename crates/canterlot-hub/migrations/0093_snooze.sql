-- «Отложить»: до этого времени сессия не будит владельца итогом хода и уходит
-- в свёрнутый раздел списка. Вопрос, сбой, подвисание или сообщение владельца
-- возвращают её раньше срока.
alter table conversations add column snoozed_until timestamptz;
create index conversations_snoozed on conversations(snoozed_until) where snoozed_until is not null;
alter table conversation_derived add column snoozed_until timestamptz;
