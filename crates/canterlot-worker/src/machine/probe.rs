//! Проба пути для условий будильника и файла отчёта автопинка.
//!
//! Проверка та же, что у `share_file` (`share::open_checked`): родители раскрыты,
//! финал открыт от дескриптора без ссылок, только обычный файл этого пользователя
//! с одной жёсткой ссылкой, под одним из `allowed_workdir_roots`, имена из запретного
//! списка (включая `.gemini`) не отдаются. Наружу уходят только факт, размер, время
//! и, если попросили, начало текста: путь и содержимое — данные уровня сообщения.

use std::{
    fs,
    io::{ErrorKind, Read},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

use canterlot_proto::{
    limits::{MAX_PROBE_HEAD_BYTES, MAX_PROBE_PATH_BYTES},
    worker::v1::{PathProbe, PathProbeError, PathProbeExists, PathProbeNotAllowed, path_probe},
};

use super::share::{self, ShareError};

/// Что машина рассказала о пути.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Exists {
        size: u64,
        mtime_ms: u64,
        /// Первые байты, только если просили и файл текстовый.
        head: Option<String>,
        /// Почему `head` пуст: `binary`, `not_requested` или `unreadable`.
        head_omitted: &'static str,
    },
    Missing,
    /// Путь не проходит защиту `share_file`; причина — короткий код, не текст.
    NotAllowed(&'static str),
    /// Проба не смогла прочитать файл: путь при этом допустим.
    Error(&'static str),
}

/// Срок пробы на воркере: зависшее чтение отвечает ошибкой, а не ждёт хаб.
pub const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Проверить путь и, если файл есть, отдать его факты. Работа блокирующая:
/// вызывающий уводит её в отдельный поток.
pub fn probe(path: &str, roots: &[PathBuf], read_head_bytes: u32) -> Outcome {
    let invalid = lexical_problem(path);
    if let Some(reason) = invalid {
        return Outcome::NotAllowed(reason);
    }
    let checked = match share::open_checked(Path::new(path), roots, ShareError::OutsideWorkdir) {
        Ok(checked) => checked,
        Err(ShareError::Missing) => return Outcome::Missing,
        Err(ShareError::OutsideWorkdir) => return Outcome::NotAllowed("outside_roots"),
        Err(ShareError::Unreadable) => return Outcome::Error("unreadable"),
        Err(ShareError::TooLarge) => return Outcome::Error("too_large"),
        Err(error) => return Outcome::NotAllowed(error.probe_reason()),
    };
    let size = checked.meta.len();
    let mtime_ms = u64::try_from(checked.meta.mtime())
        .unwrap_or(0)
        .saturating_mul(1000)
        .saturating_add(u64::try_from(checked.meta.mtime_nsec()).unwrap_or(0) / 1_000_000);
    let (head, head_omitted) = read_head(checked.file, read_head_bytes);
    Outcome::Exists {
        size,
        mtime_ms,
        head,
        head_omitted,
    }
}

/// Срок пробы: блокирующее чтение уводится в отдельный поток, а ответ приходит
/// не позже срока. Зависший поток остаётся висеть, но управляющий цикл свободен.
pub(crate) async fn run(path: String, roots: Vec<PathBuf>, read_head_bytes: u32) -> Outcome {
    with_deadline(PROBE_DEADLINE, async move {
        match tokio::task::spawn_blocking(move || probe(&path, &roots, read_head_bytes)).await {
            Ok(outcome) => outcome,
            Err(_) => Outcome::Error("internal"),
        }
    })
    .await
}

async fn with_deadline<F: std::future::Future<Output = Outcome>>(
    deadline: std::time::Duration,
    work: F,
) -> Outcome {
    match tokio::time::timeout(deadline, work).await {
        Ok(outcome) => outcome,
        Err(_) => Outcome::Error("deadline"),
    }
}

/// Ответ управляющего канала: просьба и результат связаны `request_id`.
pub fn reply(request_id: String, outcome: Outcome) -> PathProbe {
    let outcome = match outcome {
        Outcome::Exists {
            size,
            mtime_ms,
            head,
            head_omitted,
        } => path_probe::Outcome::Exists(PathProbeExists {
            size,
            mtime_ms,
            head: head.unwrap_or_default(),
            head_omitted: head_omitted.to_owned(),
        }),
        Outcome::Missing => {
            path_probe::Outcome::Missing(canterlot_proto::worker::v1::PathProbeMissing {})
        }
        Outcome::NotAllowed(reason) => path_probe::Outcome::NotAllowed(PathProbeNotAllowed {
            reason: reason.to_owned(),
        }),
        Outcome::Error(reason) => path_probe::Outcome::Error(PathProbeError {
            reason: reason.to_owned(),
        }),
    };
    PathProbe {
        request_id,
        outcome: Some(outcome),
    }
}

/// Лексические правила, которые видно без файловой системы. Относительный путь
/// уже разрешён хабом от рабочего каталога сессии.
fn lexical_problem(path: &str) -> Option<&'static str> {
    if path.is_empty() || path.len() > MAX_PROBE_PATH_BYTES || path.contains('\0') {
        return Some("invalid_path");
    }
    let path = Path::new(path);
    if !path.is_absolute() {
        return Some("not_absolute");
    }
    if path.components().any(|part| part == Component::ParentDir) {
        return Some("parent_dir");
    }
    None
}

/// Начало файла: не больше просьбы и потолка протокола, только текст.
/// Незавершённый последний символ отбрасывается; двоичный файл хвоста не отдаёт.
fn read_head(mut file: fs::File, read_head_bytes: u32) -> (Option<String>, &'static str) {
    if read_head_bytes == 0 {
        return (None, "not_requested");
    }
    let limit = read_head_bytes.min(MAX_PROBE_HEAD_BYTES) as usize;
    let mut bytes = vec![0; limit];
    let mut read = 0;
    while read < limit {
        match file.read(&mut bytes[read..]) {
            Ok(0) => break,
            Ok(count) => read += count,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return (None, "unreadable"),
        }
    }
    bytes.truncate(read);
    match text_prefix(&bytes) {
        Some(text) => (Some(text.to_owned()), ""),
        None => (None, "binary"),
    }
}

/// Допустимый префикс UTF-8: обрезанный на границе символ в конце — не двоичность.
fn text_prefix(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) {
        return None;
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).ok()
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{MetadataExt, symlink};

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn roots(root: &Path) -> Vec<PathBuf> {
        vec![root.to_path_buf()]
    }

    fn probe_in(root: &Path, name: &str, head: u32) -> Outcome {
        probe(&root.join(name).to_string_lossy(), &roots(root), head)
    }

    #[test]
    fn a_missing_file_is_missing_and_a_directory_is_not_a_file() {
        let dir = dir();
        assert_eq!(probe_in(dir.path(), "gate.rc", 0), Outcome::Missing);
        fs::create_dir(dir.path().join("sub")).unwrap();
        assert_eq!(
            probe_in(dir.path(), "sub", 0),
            Outcome::NotAllowed("not_regular")
        );
    }

    #[test]
    fn only_files_under_the_allowed_roots_are_probed() {
        let root = dir();
        let other = dir();
        fs::write(other.path().join("gate.rc"), b"x").unwrap();
        let path = other.path().join("gate.rc");
        assert_eq!(
            probe(&path.to_string_lossy(), &roots(root.path()), 0),
            Outcome::NotAllowed("outside_roots")
        );
    }

    #[test]
    fn relative_paths_and_parent_steps_are_refused() {
        let dir = dir();
        assert_eq!(
            probe("gate.rc", &roots(dir.path()), 0),
            Outcome::NotAllowed("not_absolute")
        );
        let path = format!("{}/../gate.rc", dir.path().display());
        assert_eq!(
            probe(&path, &roots(dir.path()), 0),
            Outcome::NotAllowed("parent_dir")
        );
    }

    #[test]
    fn a_link_in_the_last_component_is_refused() {
        let dir = dir();
        fs::write(dir.path().join("real"), b"x").unwrap();
        symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
        assert_eq!(
            probe_in(dir.path(), "link", 0),
            Outcome::NotAllowed("symlink")
        );
    }

    #[test]
    fn a_symlinked_parent_outside_the_roots_is_refused() {
        let root = dir();
        let outside = dir();
        fs::write(outside.path().join("gate.rc"), b"x").unwrap();
        symlink(outside.path(), root.path().join("alias")).unwrap();
        assert_eq!(
            probe_in(root.path(), "alias/gate.rc", 0),
            Outcome::NotAllowed("outside_roots")
        );
    }

    #[test]
    fn forbidden_paths_answer_not_allowed_whether_the_file_exists_or_not() {
        let root = dir();
        let outside = dir();
        // Секретный родитель есть и файл в нём есть.
        fs::create_dir(root.path().join(".ssh")).unwrap();
        fs::write(root.path().join(".ssh").join("id_ed25519"), b"secret").unwrap();
        // Секретный родитель есть, файла в нём нет.
        fs::write(root.path().join(".ssh").join("present"), b"secret").unwrap();
        for name in ["id_ed25519", "absent"] {
            assert_eq!(
                probe_in(root.path(), &format!(".ssh/{name}"), 0),
                Outcome::NotAllowed("denied"),
                "{name}"
            );
        }
        // Секретного родителя нет вовсе: ответ тот же, без похода в файловую систему.
        assert_eq!(
            probe_in(root.path(), ".ssh/missing/sentinel", 0),
            Outcome::NotAllowed("denied")
        );
        // Запретный родитель вне корней: ответ не зависит от существования файла.
        fs::write(outside.path().join("gate.rc"), b"x").unwrap();
        for name in ["gate.rc", "absent"] {
            assert_eq!(
                probe(
                    &outside.path().join(name).to_string_lossy(),
                    &roots(root.path()),
                    0
                ),
                Outcome::NotAllowed("outside_roots"),
                "{name}"
            );
        }
        assert_eq!(
            probe(
                &outside.path().join("missing/sentinel").to_string_lossy(),
                &roots(root.path()),
                0
            ),
            Outcome::NotAllowed("outside_roots")
        );
    }

    #[test]
    fn a_symlinked_parent_outside_the_roots_is_refused_before_the_target_is_touched() {
        let root = dir();
        let outside = dir();
        symlink(outside.path(), root.path().join("alias")).unwrap();
        assert_eq!(
            probe_in(root.path(), "alias/absent.rc", 0),
            Outcome::NotAllowed("outside_roots")
        );
        assert_eq!(
            probe_in(root.path(), "alias/missing/deep/sentinel", 0),
            Outcome::NotAllowed("outside_roots")
        );
        // Цель ссылки не существует: это всё равно путь вне корней, а не «файла нет».
        let gone = dir();
        let target = gone.path().join("gone");
        symlink(&target, root.path().join("dangling")).unwrap();
        assert_eq!(
            probe_in(root.path(), "dangling/sentinel", 0),
            Outcome::NotAllowed("outside_roots")
        );
    }

    #[test]
    fn a_parent_alias_into_a_secret_directory_is_refused_for_existing_and_missing_files() {
        let root = dir();
        fs::create_dir(root.path().join(".ssh")).unwrap();
        fs::write(root.path().join(".ssh").join("id_ed25519"), b"secret").unwrap();
        symlink(root.path().join(".ssh"), root.path().join("alias")).unwrap();
        assert_eq!(
            probe_in(root.path(), "alias/id_ed25519", 0),
            Outcome::NotAllowed("denied")
        );
        assert_eq!(
            probe_in(root.path(), "alias/absent", 0),
            Outcome::NotAllowed("denied")
        );
    }

    #[test]
    fn the_shared_secret_policy_covers_new_names_and_directories() {
        let dir = dir();
        for relative in [
            ".cursor/mcp.json",
            ".codexbar/config.json",
            ".config/codexbar/config.json",
            ".config/forgejo/app.ini",
            ".config/canterlot/token",
            "id_rsa",
            "deploy.pem",
            "api.token",
            "credentials.json",
            "auth.json",
        ] {
            let path = dir.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"secret").unwrap();
            assert_eq!(
                probe(&path.to_string_lossy(), &roots(dir.path()), 0),
                Outcome::NotAllowed("denied"),
                "{relative}"
            );
        }
        fs::create_dir(dir.path().join("private")).unwrap();
        fs::write(dir.path().join("private").join("auth.json"), b"secret").unwrap();
        symlink(dir.path().join("private"), dir.path().join("alias")).unwrap();
        assert_eq!(
            probe_in(dir.path(), "alias/auth.json", 0),
            Outcome::NotAllowed("denied")
        );
        assert_eq!(
            probe_in(dir.path(), "alias/missing.pem", 0),
            Outcome::NotAllowed("denied")
        );
    }

    #[test]
    fn a_denied_secret_path_is_refused_even_when_it_does_not_exist() {
        let dir = dir();
        for relative in [".env", ".gemini/oauth_creds.json", ".ssh/id_ed25519"] {
            let path = dir.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            assert_eq!(
                probe(&path.to_string_lossy(), &roots(dir.path()), 0),
                Outcome::NotAllowed("denied"),
                "{relative}"
            );
        }
    }

    #[test]
    fn a_fifo_is_not_a_regular_file() {
        let dir = dir();
        let fifo = dir.path().join("pipe");
        rustix::fs::mknodat(
            rustix::fs::CWD,
            &fifo,
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_bits_truncate(0o644),
            0,
        )
        .unwrap();
        assert_eq!(
            probe_in(dir.path(), "pipe", 0),
            Outcome::NotAllowed("not_regular")
        );
    }

    #[test]
    fn head_is_only_read_when_requested_and_never_past_the_limit() {
        let dir = dir();
        fs::write(dir.path().join("gate.rc"), b"0123456789").unwrap();
        let Outcome::Exists {
            head, head_omitted, ..
        } = probe_in(dir.path(), "gate.rc", 0)
        else {
            panic!()
        };
        assert_eq!(head, None);
        assert_eq!(head_omitted, "not_requested");
        let Outcome::Exists { head, .. } = probe_in(dir.path(), "gate.rc", 4) else {
            panic!()
        };
        assert_eq!(head.as_deref(), Some("0123"));
        let Outcome::Exists { head, .. } = probe_in(dir.path(), "gate.rc", 4096) else {
            panic!()
        };
        assert_eq!(head.as_deref(), Some("0123456789"));
    }

    #[test]
    fn a_binary_head_is_omitted_with_a_note() {
        let dir = dir();
        fs::write(dir.path().join("blob"), [0u8, 1, 2, 3]).unwrap();
        let Outcome::Exists {
            head, head_omitted, ..
        } = probe_in(dir.path(), "blob", 16)
        else {
            panic!()
        };
        assert_eq!(head, None);
        assert_eq!(head_omitted, "binary");
    }

    #[test]
    fn an_incomplete_last_character_is_trimmed_and_kept_as_text() {
        let dir = dir();
        // «да» и первый байт следующей «д»: хвост не должен сделать файл двоичным.
        fs::write(dir.path().join("gate.rc"), [0xd0, 0xb4, 0xd0]).unwrap();
        let Outcome::Exists { head, .. } = probe_in(dir.path(), "gate.rc", 16) else {
            panic!()
        };
        assert_eq!(head.as_deref(), Some("д"));
    }

    #[test]
    fn size_and_mtime_come_from_the_descriptor() {
        let dir = dir();
        let path = dir.path().join("gate.rc");
        fs::write(&path, b"0\n").unwrap();
        let meta = fs::metadata(&path).unwrap();
        let Outcome::Exists { size, mtime_ms, .. } = probe_in(dir.path(), "gate.rc", 0) else {
            panic!()
        };
        assert_eq!(size, 2);
        assert_eq!(
            mtime_ms,
            u64::try_from(meta.mtime()).unwrap() * 1000
                + u64::try_from(meta.mtime_nsec()).unwrap() / 1_000_000
        );
    }

    #[tokio::test]
    async fn a_probe_is_answered_inside_its_deadline() {
        let dir = dir();
        fs::write(dir.path().join("gate.rc"), b"0\n").unwrap();
        let outcome = run(
            dir.path().join("gate.rc").display().to_string(),
            roots(dir.path()),
            16,
        )
        .await;
        assert!(matches!(outcome, Outcome::Exists { size: 2, .. }));
    }

    #[tokio::test]
    async fn a_hung_probe_answers_with_a_deadline_error() {
        let outcome =
            with_deadline(std::time::Duration::from_millis(5), std::future::pending()).await;
        assert_eq!(outcome, Outcome::Error("deadline"));
    }
}
