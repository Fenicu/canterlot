use canterlot_worker::adapter::claude_code::process::{
    CliProcess, LaunchSpec, SessionArg, Signal, StdoutLine, build_args, build_env, write_exec_files,
};
use canterlot_worker::adapter::claude_code::version::parse_version;
use canterlot_worker::config::ClaudeCodeConfig;
use canterlot_worker::containment::Containment;
use std::fs;
use std::path::Path;
use tempfile::TempDir;
use uuid::Uuid;

fn cfg() -> ClaudeCodeConfig {
    ClaudeCodeConfig::default()
}

#[tokio::test]
async fn version_parsing_and_classification() {
    assert_eq!(parse_version("2.1.310 (Claude Code)").unwrap(), (2, 1, 310));
    assert_eq!(parse_version("2.1.311 (Claude Code)").unwrap(), (2, 1, 311));
    assert_eq!(parse_version("2.1.400 (Claude Code)").unwrap(), (2, 1, 400));
    assert_eq!(parse_version("2.1.309 (Claude Code)").unwrap(), (2, 1, 309));
    assert!(parse_version("garbage").is_err());
}

#[test]
fn owner_profile_args_keep_owner_configuration() {
    let td = TempDir::new().unwrap();
    let exec_dir = td.path().join("exec");
    fs::create_dir_all(&exec_dir).unwrap();
    let workdir = td.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "claude-haiku-5-5",
        effort: "medium",
        permission_mode: "owner",
        instruction_profile: "owner",
        instructions_file: None,
    };
    let args = build_args(&spec, &cfg()).unwrap();
    let s: Vec<String> = args
        .iter()
        .map(|o| o.to_string_lossy().to_string())
        .collect();
    assert!(!s.contains(&"--setting-sources".to_string()));
    assert!(!s.contains(&"--strict-mcp-config".to_string()));
    assert!(!s.contains(&"--permission-mode".to_string()));
    let idx = s.iter().position(|x| x == "--settings").unwrap();
    assert_eq!(s[idx + 1], "{\"crossSessionInbound\":\"refuse\"}");
    assert!(s.contains(&"--mcp-config".to_string()));
}

#[test]
fn named_profile_adds_excludes_and_prompt_file_without_touching_workdir() {
    let td = TempDir::new().unwrap();
    let exec_dir = td.path().join("exec");
    fs::create_dir_all(&exec_dir).unwrap();
    let workdir = td.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    let profile = td.path().join("profile.md");
    fs::write(&profile, "hi").unwrap();
    let before: Vec<_> = fs::read_dir(&workdir).unwrap().collect();
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instruction_profile: "p",
        instructions_file: Some(&profile),
    };
    let args = build_args(&spec, &cfg()).unwrap();
    let s: Vec<String> = args
        .iter()
        .map(|o| o.to_string_lossy().to_string())
        .collect();
    let idx = s.iter().position(|x| x == "--settings").unwrap();
    assert!(s[idx + 1].contains("claudeMdExcludes"));
    assert!(s.contains(&"--append-system-prompt-file".to_string()));
    write_exec_files(&spec, &cfg(), Path::new("/usr/bin/canterlot-worker")).unwrap();
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
        let spec = LaunchSpec {
            executable: Path::new("/usr/bin/claude"),
            exec_dir: &exec_dir,
            workdir: &workdir,
            session: SessionArg::New(Uuid::nil()),
            model: "m",
            effort: "e",
            permission_mode: input,
            instruction_profile: "owner",
            instructions_file: None,
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
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "nope",
        instruction_profile: "owner",
        instructions_file: None,
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
    let spec = LaunchSpec {
        executable: Path::new("/usr/bin/claude"),
        exec_dir: &exec_dir,
        workdir: &workdir,
        session: SessionArg::New(Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instruction_profile: "owner",
        instructions_file: None,
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
    std::fs::write(
        &script,
        r#"#!/bin/sh
head -c 68157440 /dev/zero | tr '\0' a; echo
printf '%s\n' '{"type":"x"}'
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let group = canterlot_worker::containment::TreeContainment::new().unwrap();
    let spec = LaunchSpec {
        executable: &script,
        exec_dir: td.path(),
        workdir: td.path(),
        session: SessionArg::New(uuid::Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instruction_profile: "owner",
        instructions_file: None,
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

#[tokio::test]
async fn stderr_tail_keeps_the_last_64_kib() {
    let td = TempDir::new().unwrap();
    let script = td.path().join("fake.sh");
    std::fs::write(
        &script,
        r#"#!/bin/sh
head -c 102400 /dev/zero | tr '\0' b 1>&2
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let group = canterlot_worker::containment::TreeContainment::new().unwrap();
    let spec = LaunchSpec {
        executable: &script,
        exec_dir: td.path(),
        workdir: td.path(),
        session: SessionArg::New(uuid::Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instruction_profile: "owner",
        instructions_file: None,
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
    std::fs::write(
        &script,
        "#!/bin/sh
trap 'echo got-int; exit 0' INT
echo ready
while :; do sleep 0.1; done
",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Как в main: свой обработчик SIGINT сбрасывает унаследованный SIG_IGN у потомка после exec.
    let _interrupt =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let group = canterlot_worker::containment::TreeContainment::new().unwrap();
    let spec = LaunchSpec {
        executable: &script,
        exec_dir: td.path(),
        workdir: td.path(),
        session: SessionArg::New(uuid::Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instruction_profile: "owner",
        instructions_file: None,
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
    std::fs::write(
        &script,
        "#!/bin/sh
sleep 1
",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let group = canterlot_worker::containment::TreeContainment::new().unwrap();
    let spec = LaunchSpec {
        executable: &script,
        exec_dir: td.path(),
        workdir: td.path(),
        session: SessionArg::New(uuid::Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instruction_profile: "owner",
        instructions_file: None,
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

#[test]
fn default_excludes_are_absolute_home_paths() {
    let spec = LaunchSpec {
        executable: std::path::Path::new("/usr/bin/claude"),
        exec_dir: std::path::Path::new("/tmp/xd"),
        workdir: std::path::Path::new("/tmp"),
        session: SessionArg::New(uuid::Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instruction_profile: "p",
        instructions_file: Some(std::path::Path::new("/tmp/p.md")),
    };
    let cfg = ClaudeCodeConfig::default();
    let args = build_args(&spec, &cfg).unwrap();
    let s: Vec<String> = args
        .iter()
        .map(|o| o.to_string_lossy().to_string())
        .collect();
    let idx = s.iter().position(|x| x == "--settings").unwrap();
    assert!(s[idx + 1].contains("claudeMdExcludes"));
    // HOME-based absolute paths: check no literal ~
    assert!(!s[idx + 1].contains("\"~"));
}
