//! Запись «какие инструкции видел тред». Codex 0.172.0 применяет
//! `developerInstructions` только на `thread/start`: `thread/resume` и `thread/fork`
//! поле игнорируют, поэтому смена пресета доходит до модели блоком в ходе. Блок,
//! не успевший уйти до конца исполнения, иначе терялся бы вместе с памятью адаптера:
//! следующее исполнение сверяет инструкции хаба с этой записью и повторяет блок.
//!
//! Запись лежит рядом с общим домом canterlot (`<home>-threads/<thread>.json`) и
//! хранит только режим, id пресета и версию; текст и название пресета на диск не
//! попадают. Битая запись считается отсутствующей и перезаписывается следующей.

use crate::adapter::{InstructionMode, InstructionSpec};
use std::{
    ffi::OsString,
    fs::{DirBuilder, File, OpenOptions},
    io::{self, Write as _},
    os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
};
use uuid::Uuid;

/// Что видел тред: то же сравнение, что `InstructionSpec::same_effect`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Seen {
    pub mode: InstructionMode,
    pub preset_id: Option<Uuid>,
    pub revision: u64,
}

impl Seen {
    pub fn of(spec: &InstructionSpec) -> Self {
        Self {
            mode: spec.mode,
            preset_id: spec.preset_id,
            revision: spec.revision,
        }
    }

    pub fn matches(&self, spec: &InstructionSpec) -> bool {
        *self == Self::of(spec)
    }
}

/// Каталог записей одного воркера.
#[derive(Clone, Debug)]
pub struct SeenStore {
    dir: PathBuf,
}

impl SeenStore {
    /// Записи рядом с общим домом canterlot: `<home>-threads`.
    pub fn beside_home(home: &Path) -> Option<Self> {
        let mut name: OsString = home.file_name()?.to_os_string();
        name.push("-threads");
        Some(Self {
            dir: home.with_file_name(name),
        })
    }

    fn path(&self, thread_id: &str) -> Option<PathBuf> {
        valid_thread_id(thread_id).then(|| self.dir.join(format!("{thread_id}.json")))
    }

    /// Запись треда; нет, битая или чужое имя — `None`.
    pub fn read(&self, thread_id: &str) -> Option<Seen> {
        let path = self.path(thread_id)?;
        let text = std::fs::read(path).ok()?;
        serde_json::from_slice(&text).ok()
    }

    /// Атомарно: временный файл `0600`, fsync, rename, fsync каталога.
    pub fn write(&self, thread_id: &str, seen: &Seen) -> io::Result<()> {
        let Some(path) = self.path(thread_id) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "thread id is not a file name",
            ));
        };
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        let body = serde_json::to_vec(seen).map_err(io::Error::other)?;
        let temporary = self
            .dir
            .join(format!(".{thread_id}.{}.tmp", Uuid::now_v7().simple()));
        let written = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(&body)?;
            file.sync_all()?;
            std::fs::rename(&temporary, &path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        written?;
        File::open(&self.dir)?.sync_all()
    }
}

/// Имя файла из id треда: Codex даёт UUID, всё остальное не пишется.
fn valid_thread_id(thread_id: &str) -> bool {
    !thread_id.is_empty()
        && thread_id.len() <= 128
        && thread_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
