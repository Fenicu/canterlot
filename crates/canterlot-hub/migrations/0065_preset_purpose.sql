-- Назначение пресета: обычные инструкции агента или текст автопинка (B2).
-- Списки и правка видят только своё назначение, поэтому вкладка «Текст толчка»
-- не смешивается с инструкциями, а профиль сессии не может сослаться на толчок.
alter table instruction_presets add column purpose text not null default 'instructions'
    check (purpose in ('instructions', 'nudge'));
create index instruction_presets_purpose on instruction_presets (owner_id, purpose)
    where deleted_at is null;
