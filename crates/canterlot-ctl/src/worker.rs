use std::path::PathBuf;

use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MachineIds {
    pub machine_id: Uuid,
    pub installation_id: Uuid,
    pub restore_generation: Uuid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Harness {
    ClaudeCode,
    Codex,
    OpenCode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetectedAgent {
    pub harness: Harness,
    pub executable: PathBuf,
}
