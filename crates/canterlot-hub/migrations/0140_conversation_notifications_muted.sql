-- «Без уведомлений»: владелец выключает все пуши сессии и её делегатов.
-- Вопросы и разрешения ждут молча и остаются в приложении; включение обратно
-- возвращает пуши следующего события.
alter table conversations add column notifications_muted boolean not null default false;
