-- Граница выдачи: команда, выбранная outbox для отправки, больше не правится.
-- Метка ставится под замком очереди, тем же, что берёт правка, до чтения payload.
alter table commands add column dispatch_claimed_at timestamptz;
