//! Удаление хаба и `tls add-name` на той же подделке хоста, что и обновление.

// Ошибки утилиты несут запись операции, как и в самой библиотеке.
#![allow(clippy::result_large_err)]

mod hub_support;

use std::{fs, os::unix::fs::symlink, path::Path};

use canterlot_ctl::{
    cli::UninstallArgs,
    error::CtlError,
    ops::{self, OpKind, OpResult},
    runner::Cmd,
    state::InstallState,
    tls,
    uninstall::uninstall_hub,
};
use hub_support::*;
use x509_parser::{extensions::GeneralName, prelude::*};

fn typed(text: &'static str) -> impl FnMut(&str) -> Result<String, CtlError> + Send {
    move |_| Ok(text.to_owned())
}

#[tokio::test]
async fn uninstall_hub_stops_stack_and_keeps_data() {
    let fx = Fixture::new();
    let l = &fx.layout;
    let report = uninstall_hub(
        &UninstallArgs::default(),
        l,
        &fx.runner,
        &host(),
        &mut typed(""),
    )
    .await
    .unwrap();
    assert!(fx.events().iter().any(|e| e == "compose down"));
    assert!(!l.root.join("opt/canterlot").exists());
    for kept in [
        l.install_toml(),
        l.ca_dir().join("ca.key"),
        l.hub_etc().join("hub.toml"),
        l.manifest_dir().join("installation.toml"),
        l.pg_data(),
        l.backups(),
    ] {
        assert!(kept.exists(), "{} must be kept", kept.display());
    }
    assert!(report.kept.contains(&l.root.join("etc/canterlot")));
    assert!(report.kept.contains(&l.root.join("var/lib/canterlot")));
    let state = InstallState::load(l).unwrap().unwrap();
    assert!(!state.components.hub && !state.components.postgres);
    // Пути для --purge сохраняются: поздний `uninstall --purge` знает, что удалять.
    assert!(!state.created_paths.is_empty());
    let rec = ops::latest(&l.ops_dir()).unwrap().unwrap();
    assert_eq!(rec.kind, OpKind::Uninstall);
    assert_eq!(rec.result, Some(OpResult::Done));
}

#[tokio::test]
async fn uninstall_refuses_live_sessions() {
    let fx = Fixture::new();
    fx.world().live_sessions = 2;
    let err = uninstall_hub(
        &UninstallArgs::default(),
        &fx.layout,
        &fx.runner,
        &host(),
        &mut typed(""),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CtlError::LiveSessions(2)), "{err}");
    assert!(!fx.events().iter().any(|e| e == "compose down"));
    assert!(fx.layout.hub_current().exists());
    let forced = UninstallArgs {
        purge: false,
        force: true,
    };
    uninstall_hub(&forced, &fx.layout, &fx.runner, &host(), &mut typed(""))
        .await
        .unwrap();
    assert!(fx.events().iter().any(|e| e == "compose down"));
}

fn purge() -> UninstallArgs {
    UninstallArgs {
        purge: true,
        force: false,
    }
}

#[tokio::test]
async fn purge_requires_typed_host_name() {
    let fx = Fixture::new();
    let err = uninstall_hub(
        &purge(),
        &fx.layout,
        &fx.runner,
        &host(),
        &mut typed("other"),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, CtlError::Cancelled), "{err}");
    assert!(fx.events().is_empty());
    assert!(fx.layout.pg_data().exists());
    assert!(fx.layout.hub_current().exists());
}

#[tokio::test]
async fn purge_does_not_follow_symlinks() {
    let fx = Fixture::new();
    let l = &fx.layout;
    let outside = fx.dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("keep.txt"), "keep").unwrap();
    // Ссылка внутри удаляемого каталога данных указывает наружу.
    symlink(&outside, l.hub_data().join("attachments/evil")).unwrap();
    // Записанный путь сам подменён ссылкой наружу: удаляется только ссылка.
    let outside2 = fx.dir.path().join("outside2");
    fs::create_dir_all(outside2.join("sub")).unwrap();
    fs::write(outside2.join("sub/keep.txt"), "keep").unwrap();
    fs::remove_dir_all(l.caddy_data()).unwrap();
    symlink(&outside2, l.caddy_data()).unwrap();
    let mut state = InstallState::load(l).unwrap().unwrap();
    // Путь, родитель которого — ссылка, и путь вне корней установки — отказ.
    state.created_paths.push(l.caddy_data().join("sub"));
    state.created_paths.push(l.root.join("srv/elsewhere"));
    fs::create_dir_all(l.root.join("srv/elsewhere")).unwrap();
    state.save(l).unwrap();

    let report = uninstall_hub(&purge(), l, &fx.runner, &host(), &mut typed("workstation"))
        .await
        .unwrap();
    assert!(!l.hub_data().exists());
    assert!(!l.pg_data().exists());
    assert!(fs::symlink_metadata(l.caddy_data()).is_err());
    assert_eq!(
        fs::read_to_string(outside.join("keep.txt")).unwrap(),
        "keep"
    );
    assert_eq!(
        fs::read_to_string(outside2.join("sub/keep.txt")).unwrap(),
        "keep"
    );
    assert!(l.root.join("srv/elsewhere").is_dir());
    assert!(
        report.manual.contains(&l.root.join("srv/elsewhere")),
        "{report:?}"
    );
    assert!(
        report.manual.contains(&l.caddy_data().join("sub")),
        "{report:?}"
    );
    assert!(!l.root.join("etc/canterlot").exists());
}

#[tokio::test]
async fn purge_skips_adopted_dirs() {
    let fx = Fixture::new();
    let l = &fx.layout;
    let mut state = InstallState::load(l).unwrap().unwrap();
    state.adopted_paths = vec![l.pg_data(), l.manifest_dir()];
    state.save(l).unwrap();
    let report = uninstall_hub(&purge(), l, &fx.runner, &host(), &mut typed("workstation"))
        .await
        .unwrap();
    // Принятый каталог базы не удаляется; /etc/canterlot содержит принятый манифест —
    // тоже не удаляется целиком. Оба перечислены для ручного удаления.
    assert!(l.pg_data().join("..").exists());
    assert!(l.pg_data().is_dir());
    assert!(l.manifest_dir().join("installation.toml").is_file());
    assert!(report.manual.contains(&l.pg_data()), "{report:?}");
    assert!(
        report.manual.contains(&l.root.join("etc/canterlot")),
        "{report:?}"
    );
    assert!(!l.hub_data().exists());
    assert!(!l.backups().exists());
}

fn sans(pem: &str) -> Vec<String> {
    let (_, parsed) = x509_parser::pem::parse_x509_pem(pem.as_bytes()).unwrap();
    let (_, cert) = X509Certificate::from_der(&parsed.contents).unwrap();
    let san = cert.subject_alternative_name().unwrap().unwrap();
    san.value
        .general_names
        .iter()
        .map(|name| match name {
            GeneralName::DNSName(n) => n.to_string(),
            GeneralName::IPAddress(b) => b.iter().map(u8::to_string).collect::<Vec<_>>().join("."),
            other => format!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn tls_add_name_reissues_with_old_names_plus_new() {
    let fx = Fixture::new();
    let l = &fx.layout;
    let ca_before = fs::read(l.ca_dir().join("ca.key")).unwrap();
    let rec = tls::add_name("hub.example.internal", l, &fx.runner, &host())
        .await
        .unwrap();
    assert_eq!(rec.kind, OpKind::TlsAddName);
    assert_eq!(rec.result, Some(OpResult::Done));
    let cert = fs::read_to_string(l.hub_etc().join("tls/hub.crt")).unwrap();
    let names = sans(&cert);
    for want in [
        "192.0.2.10",
        "hub.example.internal",
        "localhost",
        "127.0.0.1",
    ] {
        assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
    }
    // УЦ тот же: отпечаток у воркеров не меняется.
    assert_eq!(fs::read(l.ca_dir().join("ca.key")).unwrap(), ca_before);
    let state = InstallState::load(l).unwrap().unwrap();
    assert_eq!(state.hub_names, ["192.0.2.10", "hub.example.internal"]);
    let ev = fx.events();
    let up = pos(&ev, &format!("up hub {OLD} public"));
    let probe = pos(&ev, &format!("probe ok {OLD}"));
    assert!(up < probe);
    let chown = fx
        .runner
        .calls()
        .into_iter()
        .find(|c| c.program == "chown")
        .expect("chown of the new key");
    assert!(chown.args.iter().any(|a| a.ends_with("tls/hub.key")));
    assert!(chown.args.iter().any(|a| a == "0:980"));

    // Имя уже есть — ничего не перевыпускается и хаб не пересоздаётся.
    let before = fx.count("up hub");
    tls::add_name("hub.example.internal", l, &fx.runner, &host())
        .await
        .unwrap();
    assert_eq!(fx.count("up hub"), before);
    assert_eq!(
        fs::read_to_string(l.hub_etc().join("tls/hub.crt")).unwrap(),
        cert
    );

    for bad in ["bad name", "hub/x", "", "hub.example.internal:7443", "-x"] {
        assert!(
            matches!(
                tls::add_name(bad, l, &fx.runner, &host()).await,
                Err(CtlError::BadName(_))
            ),
            "{bad}"
        );
    }
    assert!(Path::new(&l.hub_etc().join("tls/hub.key")).is_file());
}

fn worker_on_host(fx: &Fixture) {
    let mut state = InstallState::load(&fx.layout).unwrap().unwrap();
    state.components.worker = true;
    state.worker_user = Some("worker".into());
    state.worker_home = Some("/srv/worker-home".into());
    state.save(&fx.layout).unwrap();
}

fn worker_calls(fx: &Fixture) -> Vec<Cmd> {
    fx.runner
        .calls()
        .into_iter()
        .filter(|c| c.as_user.as_deref() == Some("worker"))
        .collect()
}

fn env<'a>(c: &'a Cmd, key: &str) -> Option<&'a str> {
    c.env
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn uninstall_hub_then_removes_this_hosts_worker_as_its_account() {
    let fx = Fixture::new();
    worker_on_host(&fx);
    uninstall_hub(
        &UninstallArgs::default(),
        &fx.layout,
        &fx.runner,
        &host(),
        &mut typed(""),
    )
    .await
    .unwrap();
    let calls = worker_calls(&fx);
    assert_eq!(calls.len(), 1, "{calls:#?}");
    let c = &calls[0];
    assert!(c.program.ends_with("usr/local/bin/canterlot"));
    assert_eq!(c.args, ["uninstall", "--force"]);
    assert_eq!(env(c, "HOME"), Some("/srv/worker-home"));
    assert_eq!(env(c, "XDG_RUNTIME_DIR"), Some("/run/user/1000"));
    assert_eq!(env(c, "CANTERLOT_CTL_WORKER_ONLY"), Some("1"));
    // Сначала остановлен стек хаба, потом воркер.
    let all = fx.runner.calls();
    let down = all
        .iter()
        .position(|c| c.args.iter().any(|a| a == "down"))
        .unwrap();
    let worker = all.iter().position(|c| c.as_user.is_some()).unwrap();
    assert!(down < worker);
    assert!(
        !InstallState::load(&fx.layout)
            .unwrap()
            .unwrap()
            .components
            .worker
    );
}

#[tokio::test]
async fn update_runs_worker_after_hub_success_only() {
    use canterlot_ctl::{cli::UpdateArgs, hub_update::update};
    let fx = Fixture::new();
    worker_on_host(&fx);
    let args = UpdateArgs {
        version: None,
        from_dir: Some(fx.release(NEW)),
        force: false,
        hub_chain: false,
    };
    let reports = update(args.clone(), &fx.layout, &fx.runner, &fx.src(), &host())
        .await
        .unwrap();
    assert_eq!(reports.len(), 2);
    assert!(reports.iter().all(|r| r.succeeded()));
    let calls = worker_calls(&fx);
    assert_eq!(calls.len(), 1);
    assert!(
        calls[0]
            .args
            .starts_with(&["update".into(), "--version".into(), NEW.into()])
    );
    assert!(
        calls[0].args.iter().any(|a| a == "--hub-chain"),
        "the worker step must wait for the restarted hub: {calls:#?}"
    );

    // Неудача хаба (откат) — воркер не обновляется.
    let fx = Fixture::new();
    worker_on_host(&fx);
    fx.world().fail.insert("candidate");
    let args = UpdateArgs {
        from_dir: Some(fx.release(NEW)),
        ..args
    };
    let reports = update(args, &fx.layout, &fx.runner, &fx.src(), &host())
        .await
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert!(!reports[0].succeeded());
    assert!(worker_calls(&fx).is_empty());
}

#[tokio::test]
async fn worker_step_report_shows_child_stderr() {
    use canterlot_ctl::{
        cli::UpdateArgs,
        hub_update::{UpdateReport, update},
        runner::Output,
    };
    let fx = Fixture::new();
    worker_on_host(&fx);
    fx.world().worker_output = Some(Output {
        status: 1,
        stdout: b"worker stdout line\n".to_vec(),
        stderr: b"worker stderr line\n".to_vec(),
    });
    let args = UpdateArgs {
        version: None,
        from_dir: Some(fx.release(NEW)),
        force: false,
        hub_chain: false,
    };
    let reports = update(args, &fx.layout, &fx.runner, &fx.src(), &host())
        .await
        .unwrap();
    let worker = reports
        .iter()
        .find(|r| matches!(r, UpdateReport::Worker { .. }))
        .expect("worker report");
    let text = worker.to_string();
    assert!(text.contains("worker stdout line"), "{text}");
    assert!(text.contains("worker stderr line"), "{text}");
}
