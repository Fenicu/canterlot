use canterlot_worker::adapter::claude_code::process::{
    CliProcess, LaunchError, LaunchSpec, SessionArg, Signal, StdoutLine, build_args, build_env,
    exit_cause, write_exec_files,
};
use canterlot_worker::adapter::claude_code::version::{check, parse_version};
use canterlot_worker::adapter::{InstructionMode, InstructionSpec};
use canterlot_worker::config::ClaudeCodeConfig;
use canterlot_worker::containment::Containment;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use uuid::Uuid;

#[path = "support/fake_exec.rs"]
mod fake_exec;

fn cfg() -> ClaudeCodeConfig {
    ClaudeCodeConfig::default()
}

fn owner_instructions() -> InstructionSpec {
    InstructionSpec {
        mode: InstructionMode::Owner,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}

fn preset_instructions(text: &str) -> InstructionSpec {
    InstructionSpec {
        mode: InstructionMode::Preset,
        text: text.to_string(),
        preset_id: Some(Uuid::now_v7()),
        revision: 1,
        title: "t".to_string(),
    }
}

#[tokio::test]
async fn version_parsing_and_classification() {
    assert_eq!(parse_version("2.1.310 (Claude Code)").unwrap(), (2, 1, 310));
    assert_eq!(parse_version("2.1.311 (Claude Code)").unwrap(), (2, 1, 311));
    assert_eq!(parse_version("2.1.400 (Claude Code)").unwrap(), (2, 1, 400));
    assert_eq!(parse_version("2.1.309 (Claude Code)").unwrap(), (2, 1, 309));
    assert!(parse_version("garbage").is_err());
    // Минимум 2.1.311: 2.1.310 ниже минимума, 2.1.311 проверена.
    for (text, below) in [
        ("2.1.310 (Claude Code)", true),
        ("2.1.311 (Claude Code)", false),
    ] {
        let td = TempDir::new().unwrap();
        let script = td.path().join("fake.sh");
        fake_exec::write_executable(&script, format!("#!/bin/sh\necho '{text}'\n"), 0o755);
        let verdict = check(&script).await;
        assert_eq!(verdict.is_err(), below, "{text}: {verdict:?}");
    }
}

#[test]
fn owner_preset_args_keep_owner_configuration() {
    let td = TempDir::new().unwrap();
    let exec_dir = td.path().join("exec");
    fs::create_dir_all(&exec_dir).unwrap();
    let workdir = td.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    let owner = owner_instructions();
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "claude-haiku-5-5",
        effort: "medium",
        permission_mode: "owner",
        instructions: &owner,
    };
    let args = build_args(&spec, &cfg()).unwrap();
    let s: Vec<String> = args
        .iter()
        .map(|o| o.to_string_lossy().to_string())
        .collect();
    assert!(!s.contains(&"--setting-sources".to_string()));
    assert!(!s.contains(&"--strict-mcp-config".to_string()));
    assert!(!s.contains(&"--bare".to_string()));
    assert!(!s.contains(&"--permission-mode".to_string()));
    assert!(!s.contains(&"--append-system-prompt-file".to_string()));
    let idx = s.iter().position(|x| x == "--settings").unwrap();
    assert_eq!(s[idx + 1], "{\"crossSessionInbound\":\"refuse\"}");
    assert_eq!(s[idx + 2], "--system-prompt-snapshot");
    assert_eq!(s[idx + 3], "off");
    assert!(s.contains(&"--mcp-config".to_string()));
}

#[test]
fn no_preset_passes_bare_setting_sources_or_strict_mcp() {
    let td = TempDir::new().unwrap();
    let exec_dir = td.path().join("exec");
    fs::create_dir_all(&exec_dir).unwrap();
    let workdir = td.path().join("work");
    fs::create_dir_all(workdir.join(".git")).unwrap();
    let owner = owner_instructions();
    let clean = InstructionSpec {
        mode: InstructionMode::Clean,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    };
    let preset = preset_instructions("текст");
    for instructions in [&owner, &clean, &preset] {
        let spec = LaunchSpec {
            executable: Path::new("/usr/bin/claude"),
            exec_dir: &exec_dir,
            workdir: &workdir,
            session: SessionArg::New(Uuid::nil()),
            model: "m",
            effort: "e",
            permission_mode: "owner",
            instructions,
        };
        let args = build_args(&spec, &cfg()).unwrap();
        let s: Vec<String> = args
            .iter()
            .map(|o| o.to_string_lossy().to_string())
            .collect();
        for forbidden in ["--bare", "--setting-sources", "--strict-mcp-config"] {
            assert!(!s.contains(&forbidden.to_string()), "{forbidden}");
        }
        let idx = s.iter().position(|x| x == "--settings").unwrap();
        assert_eq!(s[idx + 2], "--system-prompt-snapshot");
        assert_eq!(s[idx + 3], "off");
    }
}

#[test]
fn custom_preset_writes_instructions_file_0600_and_appends_it() {
    let td = TempDir::new().unwrap();
    let exec_dir = td.path().join("exec");
    fs::create_dir_all(&exec_dir).unwrap();
    let workdir = td.path().join("work");
    fs::create_dir_all(workdir.join(".git")).unwrap();
    let preset = preset_instructions("секретный текст");
    let before: Vec<_> = fs::read_dir(&workdir).unwrap().collect();
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instructions: &preset,
    };
    let args = build_args(&spec, &cfg()).unwrap();
    let s: Vec<String> = args
        .iter()
        .map(|o| o.to_string_lossy().to_string())
        .collect();
    let idx = s.iter().position(|x| x == "--settings").unwrap();
    assert!(s[idx + 1].contains("claudeMdExcludes"));
    let append = s
        .iter()
        .position(|x| x == "--append-system-prompt-file")
        .unwrap();
    assert_eq!(
        s[append + 1],
        exec_dir.join("instructions.md").to_string_lossy()
    );
    write_exec_files(&spec, &cfg(), Path::new("/usr/bin/canterlot-worker")).unwrap();
    let dst = exec_dir.join("instructions.md");
    assert_eq!(std::fs::read_to_string(&dst).unwrap(), "секретный текст");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&dst).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // Рабочий каталог не тронут, текст — только в exec_dir.
    let after: Vec<_> = fs::read_dir(&workdir).unwrap().collect();
    assert_eq!(before.len(), after.len());
}

#[test]
fn permission_modes_map_to_cli_values() {
    for (input, expected) in [
        ("ask", Some("manual")),
        ("bypass", Some("bypassPermissions")),
        ("read_only", Some("plan")),
        ("owner", None),
    ] {
        let td = TempDir::new().unwrap();
        let exec_dir = td.path().join("exec");
        fs::create_dir_all(&exec_dir).unwrap();
        let workdir = td.path().join("work");
        fs::create_dir_all(&workdir).unwrap();
        let owner = owner_instructions();
        let spec = LaunchSpec {
            executable: Path::new("/usr/bin/claude"),
            exec_dir: &exec_dir,
            workdir: &workdir,
            session: SessionArg::New(Uuid::nil()),
            model: "m",
            effort: "e",
            permission_mode: input,
            instructions: &owner,
        };
        let args = build_args(&spec, &cfg()).unwrap();
        let s: Vec<String> = args
            .iter()
            .map(|o| o.to_string_lossy().to_string())
            .collect();
        if let Some(v) = expected {
            let i = s.iter().position(|x| x == "--permission-mode").unwrap();
            assert_eq!(s[i + 1], *v);
        } else {
            assert!(!s.contains(&"--permission-mode".to_string()));
        }
    }
    // unknown
    let td = TempDir::new().unwrap();
    let exec_dir = td.path().join("exec");
    fs::create_dir_all(&exec_dir).unwrap();
    let workdir = td.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    let owner = owner_instructions();
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "nope",
        instructions: &owner,
    };
    assert!(build_args(&spec, &cfg()).is_err());
}

#[test]
fn environment_drops_billing_and_nested_session_variables() {
    let pairs = vec![
        ("ANTHROPIC_API_KEY".to_string(), "x".to_string()),
        ("ANTHROPIC_BASE_URL".to_string(), "x".to_string()),
        ("CLAUDECODE".to_string(), "x".to_string()),
        ("CLAUDE_CODE_ENTRYPOINT".to_string(), "x".to_string()),
        ("CANTERLOT_X".to_string(), "x".to_string()),
        ("HOME".to_string(), "/h".to_string()),
        ("PATH".to_string(), "/p".to_string()),
        ("SSH_AUTH_SOCK".to_string(), "/s".to_string()),
        ("HTTPS_PROXY".to_string(), "http://p".to_string()),
    ];
    let it = pairs
        .into_iter()
        .map(|(k, v)| (std::ffi::OsString::from(k), std::ffi::OsString::from(v)));
    let out = build_env(it);
    let keys: Vec<String> = out
        .iter()
        .map(|(k, _)| k.to_string_lossy().to_string())
        .collect();
    assert!(keys.contains(&"HOME".to_string()));
    assert!(keys.contains(&"PATH".to_string()));
    assert!(keys.contains(&"SSH_AUTH_SOCK".to_string()));
    assert!(keys.contains(&"HTTPS_PROXY".to_string()));
    assert!(keys.contains(&"DISABLE_AUTOUPDATER".to_string()));
    assert!(!keys.contains(&"ANTHROPIC_API_KEY".to_string()));
    assert!(!keys.contains(&"ANTHROPIC_BASE_URL".to_string()));
    assert!(!keys.contains(&"CLAUDECODE".to_string()));
    assert!(!keys.contains(&"CLAUDE_CODE_ENTRYPOINT".to_string()));
    assert!(!keys.contains(&"CANTERLOT_X".to_string()));
}

#[test]
fn mcp_json_points_relay_at_exec_socket() {
    let td = TempDir::new().unwrap();
    let exec_dir = td.path().join("exec");
    let workdir = td.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    let owner = owner_instructions();
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instructions: &owner,
    };
    fs::create_dir_all(&exec_dir).unwrap();
    write_exec_files(&spec, &cfg(), Path::new("/usr/bin/canterlot-worker")).unwrap();
    let raw = fs::read_to_string(exec_dir.join("mcp.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let cmd = v["mcpServers"]["canterlot"]["command"].as_str().unwrap();
    assert_eq!(cmd, "/usr/bin/canterlot-worker");
    let args = v["mcpServers"]["canterlot"]["args"].as_array().unwrap();
    assert!(args.iter().any(|x| x == "mcp-relay"));
    assert!(
        args.iter()
            .any(|x| x == exec_dir.join("mcp.sock").to_str().unwrap())
    );
    // mode 0600
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(exec_dir.join("mcp.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[tokio::test]
async fn oversized_stdout_line_is_skipped_and_reading_continues() {
    let td = TempDir::new().unwrap();
    let script = td.path().join("fake.sh");
    fake_exec::write_executable(
        &script,
        r#"#!/bin/sh
head -c 68157440 /dev/zero | tr '\0' a; echo
printf '%s\n' '{"type":"x"}'
"#,
        0o755,
    );
    let group = canterlot_worker::containment::TreeContainment::new().unwrap();
    let owner = owner_instructions();
    let spec = LaunchSpec {
        executable: &script,
        exec_dir: td.path(),
        workdir: td.path(),
        session: SessionArg::New(uuid::Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instructions: &owner,
    };
    let mut proc = CliProcess::spawn(&spec, &ClaudeCodeConfig::default(), &group)
        .await
        .unwrap();
    match proc.next_line().await {
        StdoutLine::Oversized(n) => assert!(n > 64 * 1024 * 1024),
        other => panic!("expected Oversized, got {:?}", other),
    }
    match proc.next_line().await {
        StdoutLine::Line(l) => {
            eprintln!("GOT: {:?}", l);
            assert_eq!(l, "{\"type\":\"x\"}");
        }
        other => panic!("expected Line, got {:?}", other),
    }
    assert!(matches!(proc.next_line().await, StdoutLine::Eof));
}

#[test]
fn stderr_names_only_the_known_exit_causes() {
    assert_eq!(
        exit_cause(
            b"No conversation found with session ID: 00000000-0000-4000-8000-000000000000\n"
        ),
        Some("resume_transcript_missing")
    );
    assert_eq!(
        exit_cause(b"Error: Session ID 01a1 is already in use.\n"),
        Some("session_id_in_use")
    );
    assert_eq!(exit_cause(b"TypeError: boom\n"), None);
    assert_eq!(exit_cause(b""), None);
    assert_eq!(exit_cause(&[0xd0, 0xff, 0xfe]), None);
}

#[test]
fn launch_errors_keep_their_cause() {
    assert_eq!(LaunchError::WorkdirNotDir.reason(), "workdir_not_dir");
    assert_eq!(
        LaunchError::MissingInstructions.reason(),
        "missing_instructions"
    );
    assert_eq!(
        LaunchError::UnknownPermissionMode.reason(),
        "unknown_permission_mode"
    );
    assert_eq!(
        LaunchError::SpawnFailed(std::io::Error::other("x")).reason(),
        "cli_spawn_failed"
    );
    assert_eq!(
        LaunchError::CliNotFoundInContainment.reason(),
        "cli_not_contained"
    );
    assert_eq!(
        LaunchError::Io(std::io::Error::other("x")).reason(),
        "cli_launch_io"
    );
}

#[tokio::test]
async fn stderr_tail_keeps_the_last_64_kib() {
    let td = TempDir::new().unwrap();
    let script = td.path().join("fake.sh");
    fake_exec::write_executable(
        &script,
        r#"#!/bin/sh
head -c 102400 /dev/zero | tr '\0' b 1>&2
"#,
        0o755,
    );
    let group = canterlot_worker::containment::TreeContainment::new().unwrap();
    let owner = owner_instructions();
    let spec = LaunchSpec {
        executable: &script,
        exec_dir: td.path(),
        workdir: td.path(),
        session: SessionArg::New(uuid::Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instructions: &owner,
    };
    let mut proc = CliProcess::spawn(&spec, &ClaudeCodeConfig::default(), &group)
        .await
        .unwrap();
    // wait for process exit
    let _ = proc.wait().await;
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    let tail = proc.stderr_tail();

    assert_eq!(tail.len(), 65536);
    assert!(tail.iter().all(|&b| b == b'b'));
}

#[tokio::test]
async fn sigint_reaches_the_cli() {
    let td = TempDir::new().unwrap();
    let script = td.path().join("fake.sh");
    fake_exec::write_executable(
        &script,
        "#!/bin/sh
trap 'echo got-int; exit 0' INT
echo ready
while :; do sleep 0.1; done
",
        0o755,
    );
    // Как в main: свой обработчик SIGINT сбрасывает унаследованный SIG_IGN у потомка после exec.
    let _interrupt =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let group = canterlot_worker::containment::TreeContainment::new().unwrap();
    let owner = owner_instructions();
    let spec = LaunchSpec {
        executable: &script,
        exec_dir: td.path(),
        workdir: td.path(),
        session: SessionArg::New(uuid::Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instructions: &owner,
    };
    let mut proc = CliProcess::spawn(&spec, &ClaudeCodeConfig::default(), &group)
        .await
        .unwrap();
    let wait = std::time::Duration::from_secs(10);
    // read ready
    tokio::time::timeout(wait, async {
        loop {
            match proc.next_line().await {
                StdoutLine::Line(l) if l == "ready" => break,
                StdoutLine::Eof => panic!("eof"),
                _ => {}
            }
        }
    })
    .await
    .expect("no ready line");
    proc.signal(Signal::Int).unwrap();
    // read got-int
    let got = tokio::time::timeout(wait, async {
        loop {
            match proc.next_line().await {
                StdoutLine::Line(l) if l == "got-int" => return true,
                StdoutLine::Eof => return false,
                _ => {}
            }
        }
    })
    .await
    .expect("no reaction to SIGINT");
    assert!(got);
}

#[tokio::test]
async fn spawn_registers_cli_member_with_start_ticks() {
    let td = TempDir::new().unwrap();
    let script = td.path().join("fake.sh");
    fake_exec::write_executable(
        &script,
        "#!/bin/sh
sleep 1
",
        0o755,
    );
    let group = canterlot_worker::containment::TreeContainment::new().unwrap();
    let owner = owner_instructions();
    let spec = LaunchSpec {
        executable: &script,
        exec_dir: td.path(),
        workdir: td.path(),
        session: SessionArg::New(uuid::Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instructions: &owner,
    };
    let proc = CliProcess::spawn(&spec, &ClaudeCodeConfig::default(), &group)
        .await
        .unwrap();
    assert_ne!(proc.start_ticks, 0);
    let member = group
        .members()
        .unwrap()
        .into_iter()
        .find(|m| m.pid == proc.pid)
        .unwrap();
    assert_eq!(member.start_ticks, proc.start_ticks);
    let _ = proc;
}
