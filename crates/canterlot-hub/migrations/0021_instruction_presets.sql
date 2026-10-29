CREATE TABLE instruction_presets (
  id uuid PRIMARY KEY,
  owner_id uuid NOT NULL REFERENCES owners(id),
  title text NOT NULL,
  description text NOT NULL DEFAULT '',
  revision bigint NOT NULL,
  origin text NOT NULL CHECK (origin IN ('owner','agent')),
  origin_conversation_id uuid REFERENCES conversations(id) ON DELETE SET NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  deleted_at timestamptz
);
CREATE UNIQUE INDEX instruction_presets_title ON instruction_presets(owner_id, lower(title)) WHERE deleted_at IS NULL;
CREATE TABLE instruction_preset_revisions (
  preset_id uuid NOT NULL REFERENCES instruction_presets(id),
  revision bigint NOT NULL,
  text text NOT NULL,
  edited_by text NOT NULL CHECK (edited_by IN ('owner','agent')),
  edited_conversation_id uuid REFERENCES conversations(id) ON DELETE SET NULL,
  created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
  PRIMARY KEY (preset_id, revision)
);
ALTER TABLE owners ADD COLUMN presets_seeded_at timestamptz;
