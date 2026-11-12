use crate::adapter::canterlot_mcp::{MODE_FREE_TOOLS, RELAY_SECRET_ENV, SERVER_NAME};
use crate::adapter::{InstructionMode, InstructionSpec, SettingsChange, spawn_with_retry_io};
use crate::config::ClaudeCodeConfig;
use crate::containment::Containment;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command as TokioCommand;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub enum SessionArg {
    New(Uuid),
    Resume(String),
}

/// Корень репозитория рабочего каталога: ближайший каталог (сам workdir или выше),
/// содержащий запись `.git` (каталог, файл worktree/субмодуля или ссылка); без git — сам workdir.
pub fn repository_root(workdir: &Path) -> PathBuf {
    let start = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    let mut current: Option<&Path> = Some(start.as_path());
    while let Some(dir) = current {
        if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
            return dir.to_path_buf();
        }
        current = dir.parent();
    }
    start
}

/// Экранировать glob-метасимволы скобками: `*` → `[*]` (и так далее).
fn escape_glob_segment(segment: &str) -> String {
    let mut out = String::with_capacity(segment.len());
    for ch in segment.chars() {
        match ch {
            '*' | '?' | '{' | '}' => {
                out.push('[');
                out.push(ch);
                out.push(']');
            }
            '[' => out.push_str("[[]"),
            ']' => out.push_str("[]]"),
            other => out.push(other),
        }
    }
    out
}

/// Абсолютные пути файлов инструкций владельца строго выше корня репозитория
/// плюс пользовательский уровень; файлы самого репозитория никогда не входят.
/// Пользовательский каталог — `CLAUDE_CONFIG_DIR` окружения воркера или `~/.claude`.
pub fn owner_instruction_excludes(workdir: &Path, home: &Path) -> Vec<String> {
    let config_dir =
        crate::config::claude_config_dir(home, std::env::var_os("CLAUDE_CONFIG_DIR").as_deref());
    owner_instruction_excludes_in(workdir, home, &config_dir)
}

/// То же с явным пользовательским каталогом. CLI идёт от физического cwd
/// (`getcwd` после `chdir`), поэтому цепочка предков берётся и от логического
/// пути, и от пути с раскрытыми символическими ссылками; пользовательский
/// каталог и дом — тоже в обоих видах. Ничего не читает, кроме `.git` и ссылок.
pub fn owner_instruction_excludes_in(
    workdir: &Path,
    home: &Path,
    config_dir: &Path,
) -> Vec<String> {
    fn both(path: &Path) -> Vec<PathBuf> {
        let mut out = vec![path.to_path_buf()];
        if let Ok(real) = std::fs::canonicalize(path)
            && real != path
        {
            out.push(real);
        }
        out
    }
    let mut roots = vec![repository_root(workdir)];
    if let Ok(real) = std::fs::canonicalize(workdir) {
        let root = repository_root(&real);
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let mut out: Vec<String> = Vec::new();
    let mut add = |entry: String| {
        if !out.contains(&entry) {
            out.push(entry);
        }
    };
    for root in &roots {
        // Каталоги строго выше корня репозитория, вплоть до `/` включительно.
        let mut above: Option<&Path> = root.parent();
        while let Some(dir) = above {
            let escaped = escape_glob_segment(&dir.display().to_string());
            for suffix in [
                "CLAUDE.md",
                "CLAUDE.local.md",
                ".claude/CLAUDE.md",
                "AGENTS.md",
                ".claude/AGENTS.md",
                ".claude/rules/**",
            ] {
                // Корень `/` склеивается без двойного слеша.
                add(if dir == Path::new("/") {
                    format!("/{suffix}")
                } else {
                    format!("{escaped}/{suffix}")
                });
            }
            above = dir.parent();
        }
    }
    // Пользовательский уровень — для любого workdir.
    let mut user_dirs = both(&home.join(".claude"));
    for dir in both(config_dir) {
        if !user_dirs.contains(&dir) {
            user_dirs.push(dir);
        }
    }
    for dir in &user_dirs {
        let escaped = escape_glob_segment(&dir.display().to_string());
        for suffix in ["CLAUDE.md", "rules/**"] {
            add(format!("{escaped}/{suffix}"));
        }
    }
    // Личные файлы владельца — даже если корень репозитория это сам $HOME.
    let homes = both(home);
    if roots.iter().any(|root| homes.contains(root)) {
        for home in &homes {
            let escaped = escape_glob_segment(&home.display().to_string());
            for name in ["CLAUDE.md", "CLAUDE.local.md", "AGENTS.md"] {
                add(format!("{escaped}/{name}"));
            }
        }
    }
    out
}

/// Разрешения `permissions.allow` для инструментов [`MODE_FREE_TOOLS`]: в `ask` (`manual`)
/// CLI иначе спрашивал бы владельца о каждом вызове, даже с `readOnlyHint` (проверено вживую
/// на Claude Code 2.1.319). Режим `plan` эти правила не слушает, там работает `readOnlyHint`
/// набора Claude (`tools_list`). Остальные инструменты canterlot решает режим.
pub fn mode_free_tools_allowed() -> Vec<String> {
    MODE_FREE_TOOLS
        .iter()
        .map(|tool| format!("mcp__{SERVER_NAME}__{tool}"))
        .collect()
}

/// JSON для `--settings` по режиму инструкций.
pub fn settings_json(mode: InstructionMode, excludes: &[String]) -> serde_json::Value {
    let mut settings = serde_json::json!({
        "crossSessionInbound": "refuse",
        "permissions": {"allow": mode_free_tools_allowed()},
    });
    if mode != InstructionMode::Owner {
        settings["claudeMdExcludes"] = serde_json::json!(excludes);
        settings["autoMemoryEnabled"] = serde_json::json!(false);
    }
    settings
}

/// Текст пресета в `<exec_dir>/instructions.md` (0600); Clean/Owner удаляют устаревший файл.
pub fn write_instructions_file(
    exec_dir: &Path,
    spec: &InstructionSpec,
) -> Result<Option<PathBuf>, LaunchError> {
    let path = exec_dir.join("instructions.md");
    if spec.mode != InstructionMode::Preset {
        if std::fs::symlink_metadata(&path).is_ok() {
            std::fs::remove_file(&path).map_err(LaunchError::Io)?;
        }
        return Ok(None);
    }
    if spec.text.is_empty() {
        return Err(LaunchError::MissingInstructions);
    }
    std::fs::create_dir_all(exec_dir).map_err(LaunchError::Io)?;
    let tmp = exec_dir.join(format!("instructions.md.tmp-{}", std::process::id()));
    std::fs::write(&tmp, spec.text.as_bytes()).map_err(LaunchError::Io)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
        .map_err(LaunchError::Io)?;
    std::fs::rename(&tmp, &path).map_err(LaunchError::Io)?;
    Ok(Some(path))
}

/// Смена инструкций требует перезапуска CLI между ходами.
pub fn needs_restart(effective: &InstructionSpec, change: &SettingsChange) -> bool {
    change
        .instructions
        .as_ref()
        .is_some_and(|next| !next.same_effect(effective))
}

#[derive(Debug)]
pub enum LaunchError {
    UnknownPermissionMode,
    MissingInstructions,
    WorkdirNotDir,
    SpawnFailed(std::io::Error),
    CliNotFoundInContainment,
    Io(std::io::Error),
}

impl LaunchError {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::UnknownPermissionMode => "unknown_permission_mode",
            Self::MissingInstructions => "missing_instructions",
            Self::WorkdirNotDir => "workdir_not_dir",
            Self::SpawnFailed(_) => "cli_spawn_failed",
            Self::CliNotFoundInContainment => "cli_not_contained",
            Self::Io(_) => "cli_launch_io",
        }
    }
}

/// Причина выхода CLI по хвосту stderr: только категория, сам текст нигде не остаётся.
pub fn exit_cause(stderr: &[u8]) -> Option<&'static str> {
    let text = String::from_utf8_lossy(stderr);
    if text.contains("No conversation found with session ID") {
        Some("resume_transcript_missing")
    } else if text.contains("Session ID") && text.contains("is already in use") {
        Some("session_id_in_use")
    } else {
        None
    }
}

#[derive(Debug)]
pub enum StdoutLine {
    Line(String),
    Oversized(usize),
    Eof,
}

#[derive(Debug, Clone, Copy)]
pub enum Signal {
    Int,
    Term,
    Kill,
}

pub struct LaunchSpec<'a> {
    pub executable: &'a Path,
    pub exec_dir: &'a Path,
    pub workdir: &'a Path,
    pub session: SessionArg,
    pub model: &'a str,
    pub effort: &'a str,
    pub permission_mode: &'a str,
    pub instructions: &'a InstructionSpec,
    /// Делегированная сессия: только MCP-сервер canterlot из `mcp.json`.
    pub isolate_mcp: bool,
    /// `--prompt-suggestions`: хаб включил подсказки, сессия не делегат, CLI их знает.
    pub prompt_suggestions: bool,
}

pub fn map_permission_mode(m: &str) -> Result<Option<&'static str>, LaunchError> {
    match m {
        "owner" => Ok(None),
        "ask" => Ok(Some("manual")),
        "bypass" => Ok(Some("bypassPermissions")),
        "read_only" => Ok(Some("plan")),
        _ => Err(LaunchError::UnknownPermissionMode),
    }
}

pub fn build_args(
    spec: &LaunchSpec,
    _cfg: &ClaudeCodeConfig,
) -> Result<Vec<OsString>, LaunchError> {
    if !spec.workdir.is_dir() {
        return Err(LaunchError::WorkdirNotDir);
    }
    if spec.instructions.mode == InstructionMode::Preset && spec.instructions.text.is_empty() {
        return Err(LaunchError::MissingInstructions);
    }
    let mut args: Vec<OsString> = vec![
        "-p".into(),
        "--input-format".into(),
        "stream-json".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--include-partial-messages".into(),
        "--replay-user-messages".into(),
        "--mcp-config".into(),
        spec.exec_dir.join("mcp.json").into_os_string(),
        "--permission-prompt-tool".into(),
        "mcp__canterlot__approve".into(),
        "--model".into(),
        spec.model.into(),
        "--effort".into(),
        spec.effort.into(),
    ];
    // Серверы пользователя, проекта и плагинов не грузятся: остаётся только `mcp.json`.
    if spec.isolate_mcp {
        args.push("--strict-mcp-config".into());
    }
    // Домашний каталог — из окружения воркера; файлы не читаются, только пути.
    let home = std::env::var("HOME").unwrap_or_default();
    let excludes = owner_instruction_excludes(spec.workdir, Path::new(&home));
    let settings = settings_json(spec.instructions.mode, &excludes);
    args.push("--settings".into());
    args.push(settings.to_string().into());
    args.push("--system-prompt-snapshot".into());
    args.push("off".into());
    if let Some(mode) = map_permission_mode(spec.permission_mode)? {
        args.push("--permission-mode".into());
        args.push(mode.into());
    }
    match &spec.session {
        SessionArg::New(id) => {
            args.push("--session-id".into());
            args.push(id.to_string().into());
        }
        SessionArg::Resume(id) => {
            args.push("--resume".into());
            args.push(id.clone().into());
        }
    }
    if spec.prompt_suggestions && !spec.isolate_mcp {
        args.push("--prompt-suggestions".into());
        args.push("true".into());
    }
    if spec.instructions.mode == InstructionMode::Preset {
        args.push("--append-system-prompt-file".into());
        args.push(spec.exec_dir.join("instructions.md").into_os_string());
    }
    Ok(args)
}

pub fn build_env(parent: impl Iterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    let mut out = Vec::new();
    for (k, v) in parent {
        let ks = k.to_string_lossy();
        let drop = ks == "ANTHROPIC_API_KEY"
            || ks == "ANTHROPIC_AUTH_TOKEN"
            || ks == "ANTHROPIC_BASE_URL"
            || ks == "CLAUDECODE"
            || ks.starts_with("CLAUDE_CODE_")
            || ks.starts_with("CANTERLOT_");
        if !drop {
            out.push((k, v));
        }
    }
    out.push(("DISABLE_AUTOUPDATER".into(), "1".into()));
    // Без первого CLI 2.1.311 прячет список задач от моделей новее claude-*-4; второй оставляет
    // TodoWrite с полным списком вместо инкрементальных TaskCreate/TaskUpdate.
    out.push(("CLAUDE_CODE_ENABLE_TODO_TOOLS".into(), "true".into()));
    out.push(("CLAUDE_CODE_ENABLE_TASKS".into(), "false".into()));
    out
}

/// Коннекторы аккаунта claude.ai тоже MCP-серверы владельца: переменная их выключает.
pub fn apply_mcp_isolation(envs: &mut Vec<(OsString, OsString)>) {
    envs.retain(|(key, _)| key != "ENABLE_CLAUDEAI_MCP_SERVERS");
    envs.push(("ENABLE_CLAUDEAI_MCP_SERVERS".into(), "false".into()));
}

/// Серверы MCP владельца, разрешённые делегату, из его пользовательского файла.
#[derive(Default)]
pub struct OwnerMcpServers {
    /// Записи `mcpServers.<имя>` как есть; содержат секреты и никуда, кроме `mcp.json`, не идут.
    pub servers: serde_json::Map<String, serde_json::Value>,
    /// Разрешённые имена, которых у владельца нет.
    pub missing: Vec<String>,
}

impl std::fmt::Debug for OwnerMcpServers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnerMcpServers")
            .field("servers", &self.servers.keys().collect::<Vec<_>>())
            .field("missing", &self.missing)
            .finish()
    }
}

/// Пользовательский `~/.claude.json` владельца: явный путь или окружение воркера.
pub fn owner_user_config(cfg: &ClaudeCodeConfig) -> Option<PathBuf> {
    if let Some(path) = &cfg.owner_user_config {
        return Some(path.clone());
    }
    let home = std::env::home_dir()?;
    Some(crate::config::claude_user_config_file(
        &home,
        std::env::var_os("CLAUDE_CONFIG_DIR").as_deref(),
    ))
}

/// Из файла берутся только записи верхнего `mcpServers` с разрешёнными именами: серверы
/// проектов и плагинов в него не попадают. Нет файла, не разобрался или нет записи —
/// имя пропущено.
pub fn read_owner_mcp_servers(path: Option<&Path>, allowed: &[String]) -> OwnerMcpServers {
    let mut config = path
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    let mut owned = config
        .as_mut()
        .and_then(|config| config.get_mut("mcpServers"))
        .and_then(serde_json::Value::as_object_mut);
    let mut out = OwnerMcpServers::default();
    for name in crate::adapter::delegate_mcp_names(allowed) {
        match owned
            .as_mut()
            .and_then(|servers| servers.remove(&name))
            .filter(serde_json::Value::is_object)
        {
            Some(server) => {
                out.servers.insert(name, server);
            }
            None => out.missing.push(name),
        }
    }
    out
}

/// `relay_secret` — секрет сокета MCP этой сессии: relay получает его в окружении.
pub fn write_exec_files(
    spec: &LaunchSpec,
    cfg: &ClaudeCodeConfig,
    relay_exe: &Path,
    relay_secret: Option<&str>,
) -> Result<(), LaunchError> {
    write_exec_files_with(spec, cfg, relay_exe, relay_secret, &serde_json::Map::new())
}

/// Как `write_exec_files`, плюс разрешённые серверы владельца рядом с canterlot.
pub fn write_exec_files_with(
    spec: &LaunchSpec,
    _cfg: &ClaudeCodeConfig,
    relay_exe: &Path,
    relay_secret: Option<&str>,
    delegate_servers: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), LaunchError> {
    std::fs::create_dir_all(spec.exec_dir).map_err(LaunchError::Io)?;
    let env: serde_json::Map<String, serde_json::Value> = relay_secret
        .map(|secret| (RELAY_SECRET_ENV.to_owned(), secret.into()))
        .into_iter()
        .collect();
    let mut servers = delegate_servers.clone();
    servers.remove(SERVER_NAME);
    servers.insert(
        SERVER_NAME.to_owned(),
        serde_json::json!({
            "type": "stdio",
            "command": relay_exe.display().to_string(),
            "args": ["mcp-relay", "--socket", spec.exec_dir.join("mcp.sock").display().to_string()],
            "env": env
        }),
    );
    let mcp = serde_json::json!({ "mcpServers": servers });
    write_private(&spec.exec_dir.join("mcp.json"), mcp.to_string().as_bytes())
        .map_err(LaunchError::Io)?;
    write_instructions_file(spec.exec_dir, spec.instructions)?;
    Ok(())
}

/// Файл с секретами: 0600 с момента создания, права прежнего файла поправляются до записи.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)
}

pub struct CliProcess {
    pub pid: u32,
    pub start_ticks: u64,
    child: tokio::process::Child,
    stdin: Option<tokio::process::ChildStdin>,
    stdout: Option<tokio::io::BufReader<tokio::process::ChildStdout>>,
    // Недочитанная строка живёт здесь, а не в локальной переменной: отмена next_line ничего не теряет.
    partial: Vec<u8>,
    discarded: Option<usize>,
    stderr_ring: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
    /// Запись строки не дошла до конца (отмена по сроку или ошибка): в pipe может
    /// остаться половина кадра, следующий кадр склеился бы с ней.
    stdin_torn: bool,
}

impl CliProcess {
    pub async fn spawn(
        spec: &LaunchSpec<'_>,
        cfg: &ClaudeCodeConfig,
        group: &dyn Containment,
    ) -> Result<Self, LaunchError> {
        let args = build_args(spec, cfg)?;
        let mut envs = build_env(std::env::vars_os());
        if spec.isolate_mcp {
            apply_mcp_isolation(&mut envs);
        }
        // Команда собирается заново на каждую попытку: повтор нужен только
        // для ETXTBSY при запуске свежезаписанного двойника в тестах.
        let mut child = spawn_with_retry_io(|| {
            let mut cmd = TokioCommand::new(spec.executable);
            cmd.env_clear()
                .kill_on_drop(true)
                .args(&args)
                .current_dir(spec.workdir)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            for (k, v) in &envs {
                cmd.env(k, v);
            }
            group.prepare_command(&mut cmd);
            async move { cmd.spawn() }
        })
        .await
        .map_err(LaunchError::SpawnFailed)?;
        let child_pid = child
            .id()
            .ok_or(LaunchError::SpawnFailed(std::io::Error::other("no pid")))?;
        // Ребёнок известен без опроса дерева: опрос мог застать быстро вышедший
        // CLI уже зомби и исключить его из участников.
        let member = group
            .direct_member(child_pid)
            .map_err(|error| match error {
                crate::containment::ContainmentError::InvalidStat => {
                    LaunchError::CliNotFoundInContainment
                }
                error => LaunchError::Io(std::io::Error::other(error.to_string())),
            })?;
        group.register_cli(&member);
        let pid = member.pid;
        let start_ticks = member.start_ticks;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().map(tokio::io::BufReader::new);
        let stderr = child.stderr.take();
        let ring = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stderr_task = stderr.map(|mut se| {
            let ringc = ring.clone();
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut buf = [0u8; 4096];
                loop {
                    match se.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => {
                            let mut r = ringc.lock().unwrap();
                            r.extend_from_slice(&buf[..n]);
                            if r.len() > 64 * 1024 {
                                let start = r.len() - 64 * 1024;
                                r.drain(..start);
                            }
                        }
                        Err(_) => break,
                    }
                }
            })
        });
        Ok(Self {
            pid,
            start_ticks,
            child,
            stdin,
            stdout,
            partial: Vec::new(),
            discarded: None,
            stderr_ring: ring,
            stderr_task,
            stdin_torn: false,
        })
    }
    /// Строку пишет целиком или ломает stdin: после оборванной записи (отмена по сроку
    /// посреди `write_all`) кадры больше не пишутся, остаются SIGINT и перезапуск.
    pub async fn write_line(&mut self, json: &serde_json::Value) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        if self.stdin_torn {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "stdin line torn",
            ));
        }
        let Some(stdin) = &mut self.stdin else {
            return Err(std::io::Error::other("no stdin"));
        };
        let line = format!("{}\n", json);
        self.stdin_torn = true;
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await?;
        self.stdin_torn = false;
        Ok(())
    }

    pub fn stdin_torn(&self) -> bool {
        self.stdin_torn
    }
    /// Следующая строка stdout без `\n`; длиннее 64 МиБ пропускается до перевода строки.
    pub async fn next_line(&mut self) -> StdoutLine {
        use tokio::io::AsyncBufReadExt;
        const CAP: usize = 64 * 1024 * 1024;
        let Some(stdout) = &mut self.stdout else {
            return StdoutLine::Eof;
        };
        loop {
            let available = stdout.fill_buf().await.unwrap_or_default();
            if available.is_empty() {
                if let Some(len) = self.discarded.take() {
                    return StdoutLine::Oversized(len);
                }
                if self.partial.is_empty() {
                    return StdoutLine::Eof;
                }
                let line = std::mem::take(&mut self.partial);
                return StdoutLine::Line(String::from_utf8_lossy(&line).into_owned());
            }
            let newline = available.iter().position(|&byte| byte == b'\n');
            let chunk = newline.unwrap_or(available.len());
            let consumed = newline.map_or(chunk, |pos| pos + 1);
            if let Some(len) = self.discarded.as_mut() {
                *len += consumed;
            } else if self.partial.len() + chunk > CAP {
                self.discarded = Some(std::mem::take(&mut self.partial).len() + consumed);
            } else {
                self.partial.extend_from_slice(&available[..chunk]);
            }
            stdout.consume(consumed);
            if newline.is_some() {
                if let Some(len) = self.discarded.take() {
                    return StdoutLine::Oversized(len);
                }
                let line = std::mem::take(&mut self.partial);
                return StdoutLine::Line(String::from_utf8_lossy(&line).into_owned());
            }
        }
    }
    pub fn stderr_tail(&self) -> Vec<u8> {
        self.stderr_ring.lock().unwrap().clone()
    }
    /// Хвост stderr вышедшего процесса: чтение успевает дойти до конца, если pipe не держат потомки.
    pub async fn final_stderr(&mut self) -> Vec<u8> {
        if let Some(task) = self.stderr_task.take() {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(100), task).await;
        }
        self.stderr_tail()
    }
    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }
    pub fn signal(&self, sig: Signal) -> std::io::Result<()> {
        use rustix::process::{Pid, Signal as RSignal, kill_process};
        let pid = Pid::from_raw(self.pid as i32).ok_or_else(|| std::io::Error::other("bad pid"))?;
        let rsignal = match sig {
            Signal::Int => RSignal::INT,
            Signal::Term => RSignal::TERM,
            Signal::Kill => RSignal::KILL,
        };
        kill_process(pid, rsignal).map_err(std::io::Error::other)
    }
    pub fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.child.try_wait()
    }
    pub async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }
}

pub fn exit_event(status: std::process::ExitStatus) -> crate::adapter::AdapterEvent {
    use std::os::unix::process::ExitStatusExt;
    crate::adapter::AdapterEvent::ProcessExited {
        code: status.code(),
        signal: status.signal(),
    }
}
