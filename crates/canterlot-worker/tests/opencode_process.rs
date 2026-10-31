#![forbid(unsafe_code)]

//! Проверка версии и контракт CLI двойника `fake-opencode` (L1).

use canterlot_worker::adapter::{
    cli_version::{CliVersion, VersionError},
    opencode::version,
};
use std::{path::Path, time::Duration};

#[path = "support/fake_exec.rs"]
mod fake_exec;

fn cli(dir: &Path, output: &str) -> std::path::PathBuf {
    fake_exec::write_executable(
        &dir.join("opencode"),
        format!("#!/bin/sh\necho '{output}'\n"),
        0o700,
    )
}

#[test]
fn parses_the_cli_prefix_and_bare_numbers() {
    assert_eq!(version::parse_version("opencode v2.1.4\n"), Ok((2, 1, 4)));
    assert_eq!(version::parse_version("2.1.4"), Ok((2, 1, 4)));
    assert_eq!(
        version::parse_version("opencode"),
        Err(VersionError::Unparsable)
    );
}

#[tokio::test]
async fn verified_version_passes() {
    let dir = tempfile::tempdir().unwrap();
    let exe = cli(dir.path(), "opencode v2.1.4");
    assert_eq!(
        version::check(&exe, &[]).await,
        Ok(CliVersion::Verified("2.1.4".into()))
    );
}

#[tokio::test]
async fn newer_version_is_untested_not_refused() {
    let dir = tempfile::tempdir().unwrap();
    let exe = cli(dir.path(), "opencode v2.2.0");
    assert_eq!(
        version::check(&exe, &[]).await,
        Ok(CliVersion::UntestedNewer("2.2.0".into()))
    );
}

#[tokio::test]
async fn older_versions_are_below_the_minimum() {
    let dir = tempfile::tempdir().unwrap();
    for (line, label) in [("opencode v2.1.3", "2.1.3"), ("1.14.28", "1.14.28")] {
        let exe = cli(dir.path(), line);
        assert_eq!(
            version::check(&exe, &[]).await,
            Err(VersionError::BelowMinimum(label.into()))
        );
    }
}

#[tokio::test]
async fn missing_binary_and_failed_run_are_distinct() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        version::check(&dir.path().join("absent"), &[]).await,
        Err(VersionError::NotFound)
    );
    let exe = fake_exec::write_executable(&dir.path().join("broken"), "#!/bin/sh\nexit 3\n", 0o700);
    assert_eq!(version::check(&exe, &[]).await, Err(VersionError::Failed));
}

#[tokio::test]
async fn hung_version_check_is_cut_off_with_its_group() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("pid");
    let exe = fake_exec::write_executable(
        &dir.path().join("hung"),
        format!(
            "#!/bin/sh\nsleep 30 &\necho $! > {}\nwait\n",
            marker.display()
        ),
        0o700,
    );
    let started = std::time::Instant::now();
    assert_eq!(
        version::check_within(&exe, &[], Duration::from_millis(300)).await,
        Err(VersionError::Failed)
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    let pid = std::fs::read_to_string(&marker).unwrap();
    let path = format!("/proc/{}/stat", pid.trim());
    for _ in 0..50 {
        let alive = std::fs::read_to_string(&path).is_ok_and(|stat| {
            !stat[stat.rfind(')').unwrap() + 1..]
                .trim_start()
                .starts_with('Z')
        });
        if !alive {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("version check left its child running");
}

#[cfg(feature = "synthetic-adapter")]
mod fake {
    use canterlot_worker::adapter::opencode::{
        http::Http,
        orphans, reconcile,
        replay::{Reply, Script},
    };
    use serde_json::{Value, json};
    use std::{
        collections::HashSet,
        io::{BufRead, BufReader, Write},
        process::{Child, Command, Stdio},
        time::Duration,
    };

    const PASSWORD: &str = "fake-opencode-test-password";

    struct Server {
        child: Child,
        url: String,
        _dir: tempfile::TempDir,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    impl Server {
        fn start(script: &Script) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("script.json");
            std::fs::write(&path, serde_json::to_vec(script).unwrap()).unwrap();
            let mut child = Command::new(env!("CARGO_BIN_EXE_fake-opencode"))
                .args(["--script"])
                .arg(&path)
                .args(["serve", "--stdio", "--port", "0"])
                .env("OPENCODE_PASSWORD", PASSWORD)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut line = String::new();
            BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            let ready: Value = serde_json::from_str(&line).unwrap();
            let url = ready["url"].as_str().unwrap().to_owned();
            Self {
                child,
                url,
                _dir: dir,
            }
        }

        fn wait_exit(&mut self) -> std::process::ExitStatus {
            for _ in 0..200 {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            panic!("fake-opencode did not exit");
        }
    }

    fn get(url: &str, password: Option<&str>) -> reqwest::StatusCode {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let mut request = client.get(url);
            if let Some(password) = password {
                request = request.basic_auth("opencode", Some(password));
            }
            request.send().await.unwrap().status()
        })
    }

    fn shell_list(pid: u32, stall_ms: u64) -> Reply {
        Reply {
            body: Some(json!({"data": [{
                "status": "running", "pid": pid, "metadata": {"sessionID": "ses_1"}}]})),
            stall_ms,
            ..Default::default()
        }
    }

    // Снимки идут по очереди: более ранний и медленный не затирает более новый.
    #[tokio::test(flavor = "multi_thread")]
    async fn older_shell_snapshot_never_overwrites_a_newer_one() {
        let own = std::process::id();
        let mut script = Script::default();
        script.set_route(
            "GET",
            "/api/shell",
            vec![
                shell_list(own, 300),
                Reply {
                    body: Some(json!({"data": []})),
                    ..Default::default()
                },
            ],
        );
        let server = Server::start(&script);
        let http = Http::new(server.url.clone(), PASSWORD.into())
            .await
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let journal = orphans::Journal::new(orphans::journal_path(dir.path(), "ses_1"));
        let owners = HashSet::from(["ses_1".to_owned()]);
        let older = tokio::spawn(journal.clone().record(http.clone(), owners.clone()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        journal.clone().record(http, owners).await;
        older.await.unwrap();
        let written: Value =
            serde_json::from_slice(&std::fs::read(journal.path()).unwrap()).unwrap();
        assert_eq!(written, json!([]), "the later snapshot stays");
    }

    // Бюджет хвоста считается по байтам: страница крупнее него больше не читается.
    #[tokio::test(flavor = "multi_thread")]
    async fn tail_stops_at_its_byte_budget_and_says_so() {
        let session = "ses_1";
        let mut script = Script::default();
        let page = Reply {
            body: Some(json!({
                "data": vec![json!({"id": "msg_1", "type": "assistant", "content": []}); 100],
                "cursor": {"next": "next"},
            })),
            ..Default::default()
        };
        script.set_route(
            "GET",
            &format!("/api/session/{session}/message"),
            vec![page.clone(), page],
        );
        let server = Server::start(&script);
        let http = Http::new(server.url.clone(), PASSWORD.into())
            .await
            .unwrap();
        let (messages, truncated) = reconcile::tail_within(&http, session, 1).await.unwrap();
        assert_eq!(messages.len(), 100, "one page, the second is not read");
        assert!(truncated);
    }

    #[test]
    fn version_line_comes_from_the_script() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.json");
        let script = Script {
            version: "opencode v2.1.4".into(),
            ..Default::default()
        };
        std::fs::write(&path, serde_json::to_vec(&script).unwrap()).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_fake-opencode"))
            .args(["--script"])
            .arg(&path)
            .arg("--version")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "opencode v2.1.4\n");
    }

    #[test]
    fn api_is_closed_without_the_password() {
        let server = Server::start(&Script::default());
        assert_eq!(get(&format!("{}/api/info", server.url), None), 401);
        assert_eq!(get(&format!("{}/api/info", server.url), Some("wrong")), 401);
        assert_eq!(
            get(&format!("{}/api/info", server.url), Some(PASSWORD)),
            200
        );
    }

    #[test]
    fn stdin_eof_exits_with_zero() {
        let mut server = Server::start(&Script::default());
        drop(server.child.stdin.take());
        assert_eq!(server.wait_exit().code(), Some(0));
    }

    #[test]
    fn sigterm_exits_with_130() {
        let mut server = Server::start(&Script::default());
        let pid = rustix::process::Pid::from_raw(i32::try_from(server.child.id()).unwrap());
        rustix::process::kill_process(pid.unwrap(), rustix::process::Signal::TERM).unwrap();
        assert_eq!(server.wait_exit().code(), Some(130));
        let _ = server.child.stdin.take().map(|mut stdin| stdin.flush());
    }

    #[test]
    fn script_from_capture_keeps_the_recorded_order() {
        let script = Script::from_capture("start_stream", None);
        assert_eq!(script.version, "opencode v2.1.4");
        let expected: Vec<_> = script
            .steps
            .iter()
            .filter_map(|step| step.expected_path())
            .collect();
        assert_eq!(expected.len(), 2, "session.create and prompt");
        assert_eq!(expected[0], "/api/session");
        assert!(expected[1].ends_with("/prompt"));
        assert!(script.route("GET", "/api/agent").is_some());
    }
}

mod layout {
    use canterlot_worker::{
        adapter::opencode::process::{DataLayout, LayoutError, base_env, check_root},
        config::OpenCodeConfig,
    };
    use std::{ffi::OsString, path::Path};

    fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect()
    }

    fn cfg(root: &Path) -> OpenCodeConfig {
        OpenCodeConfig {
            data_root: Some(root.to_owned()),
            ..Default::default()
        }
    }

    #[test]
    fn default_root_sits_next_to_executions() {
        let layout = DataLayout::for_execution(
            &OpenCodeConfig::default(),
            Path::new("/srv/data/executions/abc"),
        );
        assert_eq!(layout.root(), Path::new("/srv/data/opencode"));
        assert_eq!(layout.data_home(), Path::new("/srv/data/opencode/data"));
        assert_eq!(
            DataLayout::for_worker(&OpenCodeConfig::default(), Path::new("/srv/data")).root(),
            Path::new("/srv/data/opencode")
        );
    }

    #[test]
    fn owner_database_is_refused_directly_and_through_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let owner = home.join(".local/share/opencode");
        std::fs::create_dir_all(&owner).unwrap();
        let parent = env(&[("HOME", home.to_str().unwrap())]);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&owner, &link).unwrap();
        for root in [owner.join("canterlot"), link.join("canterlot")] {
            let cfg = cfg(&root);
            assert!(
                matches!(
                    check_root(&DataLayout::for_worker(&cfg, dir.path()), &cfg, &parent),
                    Err(LayoutError::Refused)
                ),
                "{}",
                root.display()
            );
        }
        // `<root>/data/opencode` совпал бы с базой владельца.
        let cfg = OpenCodeConfig {
            data_root: Some(home.join(".local/share")),
            ..Default::default()
        };
        std::fs::create_dir_all(home.join(".local/share/data/opencode")).unwrap();
        std::fs::rename(&owner, home.join(".local/share/opencode-old")).unwrap();
        std::os::unix::fs::symlink(
            home.join(".local/share/data/opencode"),
            home.join(".local/share/opencode"),
        )
        .unwrap();
        assert!(matches!(
            check_root(&DataLayout::for_worker(&cfg, dir.path()), &cfg, &parent),
            Err(LayoutError::Refused)
        ));
    }

    #[test]
    fn xdg_data_home_and_config_paths_are_refused_too() {
        let dir = tempfile::tempdir().unwrap();
        let xdg = dir.path().join("xdg");
        let parent = env(&[("XDG_DATA_HOME", xdg.to_str().unwrap())]);
        let cfg = OpenCodeConfig {
            data_root: Some(xdg.join("opencode/x")),
            ..Default::default()
        };
        assert!(matches!(
            check_root(&DataLayout::for_worker(&cfg, dir.path()), &cfg, &parent),
            Err(LayoutError::Refused)
        ));
        let shared = dir.path().join("shared");
        let cfg = OpenCodeConfig {
            data_root: Some(shared.join("mine")),
            refuse_paths: vec![shared],
            ..Default::default()
        };
        assert!(matches!(
            check_root(&DataLayout::for_worker(&cfg, dir.path()), &cfg, &[]),
            Err(LayoutError::Refused)
        ));
    }

    #[test]
    fn own_root_passes() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let parent = env(&[("HOME", home.to_str().unwrap())]);
        let cfg = cfg(&dir.path().join("own"));
        check_root(&DataLayout::for_worker(&cfg, dir.path()), &cfg, &parent).unwrap();
    }

    #[test]
    fn env_is_an_allowlist_with_data_paths_of_canterlot() {
        let layout = DataLayout::for_worker(&OpenCodeConfig::default(), Path::new("/srv/data"));
        let parent = env(&[
            ("PATH", "/usr/bin"),
            ("HOME", "/home/owner"),
            ("USER", "owner"),
            ("LANG", "ru_RU.UTF-8"),
            ("TERM", "xterm"),
            ("XDG_CONFIG_HOME", "/home/owner/.config"),
            ("XDG_DATA_HOME", "/home/owner/.local/share"),
            ("OPENCODE_API_KEY", "secret"),
            ("OPENAI_API_KEY", "secret"),
            ("CANTERLOT_TOKEN", "secret"),
            ("RUST_LOG", "debug"),
        ]);
        let built: Vec<(String, String)> = base_env(parent.into_iter(), &layout, Path::new("/t"))
            .into_iter()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect();
        let get = |wanted: &str| {
            built
                .iter()
                .find(|(key, _)| key == wanted)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get("PATH"), Some("/usr/bin"));
        assert_eq!(get("XDG_CONFIG_HOME"), Some("/home/owner/.config"));
        assert_eq!(get("XDG_DATA_HOME"), Some("/srv/data/opencode/data"));
        assert_eq!(get("XDG_STATE_HOME"), Some("/srv/data/opencode/state"));
        assert_eq!(get("XDG_CACHE_HOME"), Some("/srv/data/opencode/cache"));
        assert_eq!(get("TMPDIR"), Some("/t"));
        assert_eq!(get("OPENCODE_DISABLE_AUTOUPDATE"), Some("1"));
        for secret in [
            "OPENCODE_API_KEY",
            "OPENAI_API_KEY",
            "CANTERLOT_TOKEN",
            "RUST_LOG",
        ] {
            assert_eq!(get(secret), None);
        }
    }
}
