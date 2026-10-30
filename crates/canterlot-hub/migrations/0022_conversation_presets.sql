ALTER TABLE conversations ADD COLUMN instruction_preset_revision bigint;

UPDATE conversations SET instruction_profile = 'owner' WHERE instruction_profile <> 'clean';

ALTER TABLE conversations ALTER COLUMN instruction_profile SET DEFAULT 'clean';

ALTER TABLE conversations ADD CONSTRAINT conversations_instruction_ref CHECK (
  (instruction_profile IN ('clean','owner') AND instruction_preset_revision IS NULL) OR
  (instruction_profile ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$' AND instruction_preset_revision IS NOT NULL));

DROP TABLE IF EXISTS machine_profiles;
