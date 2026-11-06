//! Проба пути для условий будильника и файла отчёта автопинка.
//!
//! Проверка та же, что у `share_file` (`share::open_checked`): родители раскрыты,
//! финал открыт от дескриптора без ссылок, только обычный файл этого пользователя
//! с одной жёсткой ссылкой, под одним из `allowed_workdir_roots`, имена из запретного
//! списка (включая `.gemini`) не отдаются. Наружу уходят только факт, размер, время
//! и, если попросили, начало текста: путь и содержимое — данные уровня сообщения.

use std::{
    collections::VecDeque,
    fs,
    io::{ErrorKind, Read},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{Arc, OnceLock},
};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use canterlot_proto::{
    limits::{MAX_PROBE_HEAD_BYTES, MAX_PROBE_PATH_BYTES},
    worker::v1::{
        PathProbe, PathProbeError, PathProbeExists, PathProbeNotAllowed, ProbePath, path_probe,
    },
};

use super::share::{self, ShareError};

/// Что машина рассказала о пути.
#[derive(Clone, PartialEq, Eq)]
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

impl std::fmt::Debug for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exists {
                size,
                mtime_ms,
                head,
                head_omitted,
            } => f
                .debug_struct("Exists")
                .field("size", size)
                .field("mtime_ms", mtime_ms)
                .field("head_bytes", &head.as_ref().map_or(0, String::len))
                .field("head_omitted", head_omitted)
                .finish(),
            Self::Missing => f.write_str("Missing"),
            Self::NotAllowed(reason) => f.debug_tuple("NotAllowed").field(reason).finish(),
            Self::Error(reason) => f.debug_tuple("Error").field(reason).finish(),
        }
    }
}

/// Срок пробы на воркере: зависшее чтение отвечает ошибкой, а не ждёт хаб.
pub const PROBE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Столько файловых проб может висеть в потоках одновременно. Разрешение
/// освобождается только вместе с блокирующей задачей, а не по сроку ответа.
pub const MAX_CONCURRENT_PROBES: usize = 4;

/// Предел общий на процесс: он переживает переподключение управляющего канала,
/// иначе зависшие задачи старого сеанса копились бы с новыми.
static PROBE_PERMITS: OnceLock<Arc<Semaphore>> = OnceLock::new();

pub(crate) fn process_permits() -> Arc<Semaphore> {
    PROBE_PERMITS
        .get_or_init(|| Arc::new(Semaphore::new(MAX_CONCURRENT_PROBES)))
        .clone()
}

/// Очередь запросов пробы: меньше предела — ждут ответа по порядку, сверх —
/// сразу получают явный `busy` со своим `request_id`, а не теряются.
#[derive(Default)]
pub(crate) struct PendingProbes {
    queued: VecDeque<ProbePath>,
    running: usize,
    cap: usize,
}

impl PendingProbes {
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            ..Self::default()
        }
    }

    pub fn submit(&mut self, request: ProbePath) -> Result<(), ProbePath> {
        if self.running.saturating_add(self.queued.len()) >= self.cap {
            return Err(request);
        }
        self.queued.push_back(request);
        Ok(())
    }

    pub fn start_next(&mut self) -> Option<ProbePath> {
        let request = self.queued.pop_front()?;
        self.running += 1;
        Some(request)
    }

    pub fn finish(&mut self) {
        self.running = self.running.saturating_sub(1);
    }
}

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
/// не позже срока. Разрешение держит сам поток: зависшая проба не копит новые
/// потоки, а сверх предела отвечает `busy`, не дожидаясь срока.
pub(crate) async fn run(
    path: String,
    roots: Vec<PathBuf>,
    read_head_bytes: u32,
    permits: Arc<Semaphore>,
) -> Outcome {
    run_with_deadline(PROBE_DEADLINE, permits, move || {
        probe(&path, &roots, read_head_bytes)
    })
    .await
}

async fn run_with_deadline<F>(
    deadline: std::time::Duration,
    permits: Arc<Semaphore>,
    work: F,
) -> Outcome
where
    F: FnOnce() -> Outcome + Send + 'static,
{
    let Ok(permit): Result<OwnedSemaphorePermit, _> = permits.try_acquire_owned() else {
        return Outcome::Error("busy");
    };
    let work = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    });
    match tokio::time::timeout(deadline, work).await {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(_)) => Outcome::Error("internal"),
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
        now_ms: super::now_ms(),
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
    fn token_stores_are_refused_but_token_reports_are_allowed() {
        let dir = dir();
        for relative in [
            "token",
            "api.token",
            "my_token.txt",
            "access_token.json",
            "apikey",
            "deploy.secret",
            "secrets.json",
        ] {
            let path = dir.path().join(relative);
            fs::write(&path, b"secret").unwrap();
            assert_eq!(
                probe(&path.to_string_lossy(), &roots(dir.path()), 0),
                Outcome::NotAllowed("denied"),
                "{relative}"
            );
        }
        for relative in [
            "tokens-report.md",
            "token-usage.png",
            "tokenizer.rs",
            "tokens-compact.test.ts",
        ] {
            let path = dir.path().join(relative);
            fs::write(&path, b"report").unwrap();
            assert!(
                matches!(
                    probe(&path.to_string_lossy(), &roots(dir.path()), 0),
                    Outcome::Exists { .. }
                ),
                "{relative} must stay probeable"
            );
        }
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
    fn a_head_larger_than_four_kib_is_clamped_to_the_protocol_limit() {
        let dir = dir();
        fs::write(dir.path().join("big.txt"), vec![b'x'; 8 * 1024]).unwrap();
        let Outcome::Exists { head, .. } = probe_in(dir.path(), "big.txt", 32 * 1024) else {
            panic!()
        };
        let head = head.expect("text head");
        assert_eq!(head.len(), MAX_PROBE_HEAD_BYTES as usize);
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

    fn permits() -> Arc<Semaphore> {
        Arc::new(Semaphore::new(MAX_CONCURRENT_PROBES))
    }

    fn request(id: &str) -> ProbePath {
        ProbePath {
            request_id: id.into(),
            path: "/tmp/gate.rc".into(),
            read_head_bytes: 0,
        }
    }

    #[test]
    fn debug_does_not_print_the_head_or_the_path() {
        let outcome = Outcome::Exists {
            size: 2,
            mtime_ms: 1,
            head: Some("secret-token".into()),
            head_omitted: "",
        };
        let text = format!("{outcome:?}");
        assert!(!text.contains("secret-token"), "{text}");
        assert!(text.contains("head_bytes: 12"), "{text}");
    }

    #[test]
    fn probe_requests_wait_in_a_bounded_fifo() {
        let mut queue = PendingProbes::new(2);
        assert!(queue.submit(request("a")).is_ok());
        assert!(queue.submit(request("b")).is_ok());
        let overflow = queue.submit(request("c")).unwrap_err();
        assert_eq!(
            overflow.request_id, "c",
            "the busy reply keeps its request id"
        );
        assert_eq!(queue.start_next().unwrap().request_id, "a");
        queue.finish();
        assert!(
            queue.submit(request("c")).is_ok(),
            "a finished probe frees its slot"
        );
        assert_eq!(queue.start_next().unwrap().request_id, "b");
        assert_eq!(queue.start_next().unwrap().request_id, "c");
        assert!(queue.start_next().is_none());
    }

    #[tokio::test]
    async fn a_probe_is_answered_inside_its_deadline() {
        let dir = dir();
        fs::write(dir.path().join("gate.rc"), b"0\n").unwrap();
        let outcome = run(
            dir.path().join("gate.rc").display().to_string(),
            roots(dir.path()),
            16,
            permits(),
        )
        .await;
        assert!(matches!(outcome, Outcome::Exists { size: 2, .. }));
    }

    #[tokio::test]
    async fn a_hung_probe_answers_with_a_deadline_error_and_keeps_its_permit() {
        let permits = permits();
        let mut hung = Vec::new();
        for _ in 0..MAX_CONCURRENT_PROBES {
            let permits = permits.clone();
            hung.push(tokio::spawn(run_with_deadline(
                std::time::Duration::from_millis(10),
                permits,
                || {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    Outcome::Missing
                },
            )));
        }
        for task in hung {
            assert_eq!(task.await.unwrap(), Outcome::Error("deadline"));
        }
        // Ответы уже ушли, но блокирующие задачи живы и держат разрешения.
        assert!(
            permits.clone().try_acquire_owned().is_err(),
            "a timed-out task must not release its permit"
        );
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(permits.clone().try_acquire_owned().is_ok());
    }

    #[tokio::test]
    async fn work_over_the_permit_limit_is_rejected_promptly() {
        let permits = permits();
        let held: Vec<_> = (0..MAX_CONCURRENT_PROBES)
            .map(|_| permits.clone().try_acquire_owned().unwrap())
            .collect();
        let started = std::time::Instant::now();
        let outcome = run("gate.rc".into(), Vec::new(), 0, permits.clone()).await;
        assert_eq!(outcome, Outcome::Error("busy"));
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        drop(held);
    }
}
