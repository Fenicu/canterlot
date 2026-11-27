//! Обновление хаба на FakeRunner. Подделка ведёт маленький «мир»: базы кластера с oid,
//! строки миграций каждой базы, какой хаб запущен и опубликован ли gRPC. Тесты сверяют
//! порядок шагов, итог операции и фазы записи. Ни Docker, ни PostgreSQL, ни сеть не трогаются.

// Ошибки утилиты несут запись операции, как и в самой библиотеке.
#![allow(clippy::result_large_err)]

mod hub_support;

use std::{fs, path::Path};

use canterlot_ctl::{
    cli::{UpdateArgs, Version},
    error::CtlError,
    hub_update::{UpdateReport, probe, update_hub},
    ops::{self, OpDetails, OpKind, OpRecord, OpResult, Traffic},
    runner::{Cmd, FakeRunner, Output},
    web,
};
use hub_support::*;

fn finished(report: UpdateReport) -> OpRecord {
    match report {
        UpdateReport::Finished(rec) => *rec,
        other => panic!("expected a finished operation, got {other:?}"),
    }
}

fn phases(rec: &OpRecord) -> Vec<String> {
    rec.history.iter().map(|(_, p)| p.clone()).collect()
}

#[tokio::test]
async fn update_happy_path_fences_before_stop_and_admits_last() {
    let fx = Fixture::new();
    let rec = finished(fx.update().await.unwrap());
    assert_eq!(rec.result, Some(OpResult::Done), "{rec:#?}");
    assert_eq!(rec.kind, OpKind::Update);
    assert_eq!(rec.traffic, Traffic::Admitted);
    assert_eq!(rec.previous.as_ref().map(Version::as_str), Some(OLD));
    assert_eq!(rec.target.as_ref().map(Version::as_str), Some(NEW));
    let p = phases(&rec);
    let order = [
        "prepare",
        "fence",
        "dump",
        "candidate",
        "admit",
        "post",
        "done",
    ];
    let mut last = 0;
    for phase in order {
        let at = p
            .iter()
            .position(|x| x == phase)
            .unwrap_or_else(|| panic!("{phase} in {p:?}"));
        assert!(at >= last, "{phase} out of order in {p:?}");
        last = at;
    }
    assert!(!p.iter().any(|x| x.starts_with("rollback")));

    let ev = fx.events();
    // Обслуживание включено и записано до остановки старого хаба.
    let stop = pos(&ev, "stop hub");
    assert!(ev[stop].contains("maintenance=true") && ev[stop].contains("traffic=Fenced"));
    let dump = pos(&ev, "pg_dump");
    let candidate = pos(&ev, &format!("up hub {NEW} fenced"));
    let reload = pos(&ev, "caddy reload");
    let public = pos(&ev, &format!("up hub {NEW} public"));
    assert!(stop < dump && dump < candidate && candidate < reload);
    assert!(reload < public, "{ev:#?}");
    // gRPC публикуется, пока API ещё в обслуживании; флаг снимается последним.
    assert!(ev[candidate].contains("maintenance=true"));
    assert!(ev[public].contains("maintenance=true"), "{ev:#?}");
    // Старый хаб не поднимался, база не восстанавливалась.
    assert!(!ev.iter().any(|e| e.starts_with(&format!("up hub {OLD}"))));
    assert!(!ev.iter().any(|e| e.starts_with("create database")));

    let l = &fx.layout;
    assert!(!web::maintenance_flag(l).exists());
    assert_eq!(
        fs::read_link(l.web_parent().join("build")).unwrap(),
        Path::new("releases").join(NEW)
    );
    assert_eq!(
        fs::read_link(l.hub_current()).unwrap(),
        Path::new("releases").join(NEW)
    );
    assert_eq!(
        fs::read_link(l.root.join("opt/canterlot/hub/previous")).unwrap(),
        Path::new("releases").join(OLD)
    );
    // Старые чанки и их сжатые соседи слиты в новое дерево.
    let new_web = l.web_release(&v(NEW));
    assert!(new_web.join("_app/immutable/old.js").is_file());
    assert!(new_web.join("_app/immutable/old.js.br").is_file());
    assert!(new_web.join("_app/immutable/new.js").is_file());
    let caddy = fs::read_to_string(l.caddy_dir().join("Caddyfile")).unwrap();
    assert!(caddy.contains("connect-src 'self' wss://192.0.2.10 https://new.example"));
    let compose = fs::read_to_string(l.compose_dir().join("compose.yaml")).unwrap();
    assert!(compose.contains(&format!("/opt/canterlot/hub/releases/{NEW}:")));
    let dump = rec.dump.clone().expect("dump path recorded");
    assert!(dump.starts_with(l.backups()) && dump.is_file());
    assert!(rec.db_identity.is_some());
    assert_eq!(rec.details.migrations, ["1 aa 1", "2 bb 1"]);
    assert_no_live_drop(&fx.runner.calls());
    assert!(ops::unfinished(&l.ops_dir()).unwrap().is_none());
}

#[tokio::test]
async fn update_refuses_downgrade() {
    let fx = Fixture::new();
    let older = fx.release("1.2.0");
    let err = fx.update_from(&older).await.unwrap_err();
    assert!(matches!(err, CtlError::DowngradeRefused { .. }), "{err}");
    assert!(fx.events().is_empty(), "{:#?}", fx.events());
    assert!(ops::unfinished(&fx.layout.ops_dir()).unwrap().is_none());
    // Та же версия — нечего делать, ничего не останавливается.
    let same = fx.release(OLD);
    assert!(matches!(
        fx.update_from(&same).await.unwrap(),
        UpdateReport::UpToDate(_)
    ));
    assert!(fx.events().is_empty());
}

#[tokio::test]
async fn update_refuses_incompatible_migrations() {
    let fx = Fixture::new();
    fx.world().manifest = manifest(&[(1, "aa"), (2, "ff"), (3, "cc")]);
    let err = fx.update().await.unwrap_err();
    assert!(matches!(err, CtlError::MigrationsIncompatible(_)), "{err}");
    assert_untouched(&fx);
    let rec = ops::latest(&fx.layout.ops_dir()).unwrap().unwrap();
    assert!(matches!(
        rec.result,
        Some(OpResult::FailedBeforeChange { .. })
    ));
}

#[tokio::test]
async fn update_refuses_insufficient_space() {
    let fx = Fixture::new();
    {
        let mut w = fx.world();
        w.db_size = 10 * GIB;
        // 2 × база + выпуск + 1 ГиБ не помещается: 20 ГиБ свободно.
        w.free = 20 * GIB;
    }
    let err = fx.update().await.unwrap_err();
    assert!(matches!(err, CtlError::InsufficientSpace(_)), "{err}");
    assert_untouched(&fx);
}

fn assert_untouched(fx: &Fixture) {
    let ev = fx.events();
    assert!(!ev.iter().any(|e| e.starts_with("stop hub")), "{ev:#?}");
    assert!(!ev.iter().any(|e| e.starts_with("pg_dump")), "{ev:#?}");
    assert!(!web::maintenance_flag(&fx.layout).exists());
    assert_eq!(
        fs::read_link(fx.layout.hub_current()).unwrap(),
        Path::new("releases").join(OLD)
    );
}

#[tokio::test]
async fn update_dump_failure_restarts_old_hub() {
    let fx = Fixture::new();
    fx.world().fail.insert("dump");
    let rec = finished(fx.update().await.unwrap());
    assert!(
        matches!(rec.result, Some(OpResult::FailedBeforeChange { .. })),
        "{rec:#?}"
    );
    assert_eq!(rec.traffic, Traffic::Admitted);
    let ev = fx.events();
    let stop = pos(&ev, "stop hub");
    let dump = pos(&ev, "pg_dump failed");
    let restart = pos(&ev, &format!("up hub {OLD} public"));
    assert!(stop < dump && dump < restart, "{ev:#?}");
    assert!(!ev.iter().any(|e| e.starts_with(&format!("up hub {NEW}"))));
    assert!(!web::maintenance_flag(&fx.layout).exists());
    assert_eq!(fx.world().hub.as_deref(), Some(OLD));
    // Недописанный дамп удалён: место на диске возвращено.
    let leftovers: Vec<_> = fs::read_dir(fx.layout.backups())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert_eq!(
        fs::read_link(fx.layout.web_parent().join("build")).unwrap(),
        Path::new("releases").join(OLD)
    );
}

#[tokio::test]
async fn update_candidate_failure_restores_into_separate_db_and_swaps() {
    let fx = Fixture::new();
    fx.world().fail.insert("candidate");
    let rec = finished(fx.update().await.unwrap());
    let Some(OpResult::RolledBack { reason }) = &rec.result else {
        panic!("expected rolled_back, got {rec:#?}");
    };
    assert!(reason.contains("probe"), "{reason}");
    let p = phases(&rec);
    for phase in ["rollback_restore", "rollback_swap", "rollback_start"] {
        assert!(p.iter().any(|x| x == phase), "{phase} in {p:?}");
    }
    let ev = fx.events();
    let restore_db = rec.details.restore_db.clone().unwrap();
    let failed_db = rec.details.failed_db.clone().unwrap();
    let create = pos(&ev, &format!("create database {restore_db}"));
    let restore = pos(&ev, &format!("pg_restore into {restore_db}"));
    let swap = pos(
        &ev,
        &format!("swap canterlot->{failed_db} {restore_db}->canterlot"),
    );
    let old_fenced = pos(&ev, &format!("up hub {OLD} fenced"));
    let old_probe = pos(&ev, &format!("probe ok {OLD}"));
    let old_public = pos(&ev, &format!("up hub {OLD} public"));
    assert!(create < restore && restore < swap && swap < old_fenced);
    assert!(old_fenced < old_probe && old_probe < old_public);
    assert!(ev[old_public].contains("maintenance=true"), "{ev:#?}");

    {
        let w = fx.world();
        // Живая база — восстановленная копия, мигрированная хранится под именем failed.
        assert_eq!(w.dbs["canterlot"], w.dbs_created[&restore_db]);
        assert_eq!(w.dbs[&failed_db], LIVE_OID);
        assert_eq!(w.migrations["canterlot"], ["1 aa 1", "2 bb 1"]);
        assert_eq!(w.hub.as_deref(), Some(OLD));
    }
    assert_no_live_drop(&fx.runner.calls());
    assert!(
        !fx.runner
            .calls()
            .iter()
            .any(|c| stdin(c).to_lowercase().contains("drop database")),
        "rollback must not drop any database"
    );
    // Восстановленная база записана как новое тождество живой базы.
    let identity = rec.db_identity.clone().unwrap();
    assert_eq!(identity.database_oid, fx.world().dbs["canterlot"]);
    assert!(!web::maintenance_flag(&fx.layout).exists());
    assert_eq!(
        fs::read_link(fx.layout.web_parent().join("build")).unwrap(),
        Path::new("releases").join(OLD)
    );
    assert_eq!(
        fs::read_link(fx.layout.hub_current()).unwrap(),
        Path::new("releases").join(OLD)
    );
    let compose = fs::read_to_string(fx.layout.compose_dir().join("compose.yaml")).unwrap();
    assert!(compose.contains(&format!("/opt/canterlot/hub/releases/{OLD}:")));
}

#[tokio::test]
async fn update_restore_failure_leaves_live_db_and_stays_fenced() {
    let fx = Fixture::new();
    {
        let mut w = fx.world();
        w.fail.insert("candidate");
        w.fail.insert("restore");
    }
    let rec = finished(fx.update().await.unwrap());
    let Some(OpResult::NeedsOwner { steps }) = &rec.result else {
        panic!("expected needs_owner, got {rec:#?}");
    };
    let text = steps.join("\n");
    assert!(text.contains("pg_restore"), "{text}");
    assert!(
        text.contains(rec.dump.as_ref().unwrap().to_str().unwrap()),
        "{text}"
    );
    assert_eq!(rec.traffic, Traffic::Fenced);
    assert!(web::maintenance_flag(&fx.layout).exists());
    {
        let w = fx.world();
        assert_eq!(w.dbs["canterlot"], LIVE_OID);
        assert_eq!(w.hub, None, "hub must stay stopped");
    }
    let ev = fx.events();
    assert!(!ev.iter().any(|e| e.starts_with("swap")));
    assert!(!ev.iter().any(|e| e.starts_with(&format!("up hub {OLD}"))));
    let last_stop = ev.iter().rposition(|e| e.starts_with("stop hub")).unwrap();
    let last_up = ev.iter().rposition(|e| e.starts_with("up hub")).unwrap();
    assert!(last_stop > last_up, "{ev:#?}");
    assert_no_live_drop(&fx.runner.calls());

    // Следующий запуск не начинает новое обновление поверх такого состояния.
    let err = fx.update().await.unwrap_err();
    assert!(matches!(err, CtlError::NeedsOwnerPending(_)), "{err}");
}

#[tokio::test]
async fn update_after_admit_is_forward_only() {
    let fx = Fixture::new();
    // Прежний откат оставил базу failed; её удаление после успеха падает.
    let stale = "canterlot_failed_0190000000007000800000000000000a";
    fx.world().dbs.insert(stale.into(), 20000);
    let mut old = OpRecord::new(OpKind::Update, Some(v(NEW)));
    old.id = ops::OpId("01900000-0000-7000-8000-00000000000a".into());
    old.details = OpDetails {
        failed_db: Some(stale.into()),
        ..OpDetails::default()
    };
    old.result = Some(OpResult::RolledBack {
        reason: "test".into(),
    });
    fs::write(
        fx.layout.ops_dir().join(format!("{}.json", old.id)),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    fx.world().fail.insert("drop");

    let rec = finished(fx.update().await.unwrap());
    assert_eq!(rec.result, Some(OpResult::Done), "{rec:#?}");
    assert!(
        rec.details.warnings.iter().any(|w| w.contains(stale)),
        "{:?}",
        rec.details.warnings
    );
    let ev = fx.events();
    assert!(ev.iter().any(|e| e.starts_with("drop failed")), "{ev:#?}");
    assert!(!ev.iter().any(|e| e.starts_with("create database")));
    assert!(!ev.iter().any(|e| e.starts_with("swap")));
    assert!(!ev.iter().any(|e| e.starts_with(&format!("up hub {OLD}"))));
    assert_eq!(fx.world().hub.as_deref(), Some(NEW));
}

#[tokio::test]
async fn update_retention_drops_only_recorded_scratch_databases() {
    let fx = Fixture::new();
    let stale = "canterlot_failed_0190000000007000800000000000000a";
    let foreign = "canterlot_failed_0190000000007000800000000000000b";
    {
        let mut w = fx.world();
        w.dbs.insert(stale.into(), 20000);
        w.dbs.insert(foreign.into(), 20001);
    }
    let mut old = OpRecord::new(OpKind::Update, Some(v(NEW)));
    old.id = ops::OpId("01900000-0000-7000-8000-00000000000a".into());
    old.details.failed_db = Some(stale.into());
    old.result = Some(OpResult::RolledBack {
        reason: "test".into(),
    });
    fs::write(
        fx.layout.ops_dir().join(format!("{}.json", old.id)),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    let rec = finished(fx.update().await.unwrap());
    assert_eq!(rec.result, Some(OpResult::Done));
    {
        let w = fx.world();
        assert!(!w.dbs.contains_key(stale));
        // База, которой нет ни в одной записи, не трогается, даже если имя похоже.
        assert!(w.dbs.contains_key(foreign));
    }
    assert_no_live_drop(&fx.runner.calls());
}

#[tokio::test]
async fn update_resumes_recorded_operation() {
    let fx = Fixture::new();
    // Процесс умирает на первой проверке кандидата: запись остаётся на фазе candidate.
    let crashed = fx
        .update_crashing(|w, cmd| has(cmd, &["probe"]) && w.hub.as_deref() == Some(NEW))
        .await;
    assert!(crashed, "first run must crash");
    let open = ops::unfinished(&fx.layout.ops_dir()).unwrap().unwrap();
    assert_eq!(open.phase, "candidate");
    assert_eq!(open.traffic, Traffic::Fenced);
    let dumps_before = fx.count("pg_dump");
    assert_eq!(dumps_before, 1);

    // Статус видит незавершённую операцию.
    let report = canterlot_ctl::status::status(&fx.layout, &fx.runner)
        .await
        .unwrap();
    assert_eq!(
        report.running.as_ref().map(|r| r.id.clone()),
        Some(open.id.0.clone())
    );

    let rec = finished(fx.update().await.unwrap());
    assert_eq!(rec.id, open.id, "resume continues the same operation");
    assert_eq!(rec.result, Some(OpResult::Done), "{rec:#?}");
    assert_eq!(fx.count("pg_dump"), 1, "resume must not dump again");
    assert!(!web::maintenance_flag(&fx.layout).exists());

    // Другая версия при незавершённой операции — отказ, а не новая операция.
    let fx = Fixture::new();
    assert!(
        fx.update_crashing(|w, cmd| has(cmd, &["probe"]) && w.hub.as_deref() == Some(NEW))
            .await
    );
    let other = fx.release("1.4.0");
    assert!(matches!(
        fx.update_from(&other).await.unwrap_err(),
        CtlError::Unfinished(_)
    ));
}

#[tokio::test]
async fn update_resumes_interrupted_restore_and_after_swap() {
    // Сбой посреди pg_restore: продолжение заменяет недовосстановленную служебную базу.
    let fx = Fixture::new();
    fx.world().fail.insert("candidate");
    assert!(
        fx.update_crashing(
            |_, cmd| cmd.program == "sh" && cmd.args.iter().any(|a| a == "--exit-on-error")
        )
        .await
    );
    let open = ops::unfinished(&fx.layout.ops_dir()).unwrap().unwrap();
    assert_eq!(open.phase, "rollback_restore");
    let rec = finished(fx.update().await.unwrap());
    assert!(
        matches!(rec.result, Some(OpResult::RolledBack { .. })),
        "{rec:#?}"
    );
    assert_eq!(fx.count("pg_dump"), 1);
    assert_eq!(fx.count("swap"), 1);
    assert_no_live_drop(&fx.runner.calls());

    // Сбой сразу после обмена имён: продолжение видит обмен по oid и не повторяет его.
    let fx = Fixture::new();
    fx.world().fail.insert("candidate");
    assert!(
        fx.update_crashing(|w, cmd| w.swapped && stdin(cmd).contains("from pg_database"))
            .await
    );
    let open = ops::unfinished(&fx.layout.ops_dir()).unwrap().unwrap();
    assert_eq!(open.phase, "rollback_swap");
    let rec = finished(fx.update().await.unwrap());
    assert!(
        matches!(rec.result, Some(OpResult::RolledBack { .. })),
        "{rec:#?}"
    );
    assert_eq!(fx.count("swap"), 1, "swap must not run twice");
    assert_eq!(fx.world().hub.as_deref(), Some(OLD));
}

#[tokio::test]
async fn update_refuses_ambiguous_record() {
    let fx = Fixture::new();
    fx.world().fail.insert("candidate");
    assert!(
        fx.update_crashing(|w, cmd| w.swapped && stdin(cmd).contains("from pg_database"))
            .await
    );
    // Кто-то тронул базы между запусками: ни «до обмена», ни «после обмена».
    {
        let mut w = fx.world();
        let restored = w.dbs.remove("canterlot").unwrap();
        w.dbs.insert("canterlot_manual".into(), restored);
    }
    let calls_before = fx.runner.calls().len();
    let err = fx.update().await.unwrap_err();
    let CtlError::Ambiguous(rec) = err else {
        panic!("expected ambiguous, got {err}");
    };
    assert_eq!(rec.phase, "rollback_swap");
    assert!(
        !rec.details.manual.is_empty(),
        "manual check must be printed"
    );
    let later = &fx.runner.calls()[calls_before..];
    for call in later {
        let sql = stdin(call).to_lowercase();
        assert!(!sql.contains("alter database"), "{call:?}");
        assert!(!sql.contains("drop database"), "{call:?}");
        assert!(!sql.contains("create database"), "{call:?}");
        assert!(!has(call, &["up"]), "{call:?}");
    }
    // Запись не закрыта: следующий запуск снова откажет, а не начнёт новую операцию.
    assert!(ops::unfinished(&fx.layout.ops_dir()).unwrap().is_some());

    // Запись с незнакомой фазой — тоже отказ.
    let fx = Fixture::new();
    let mut rec = OpRecord::new(OpKind::Update, Some(v(NEW)));
    rec.phase = "mystery".into();
    ops::begin(&fx.layout.ops_dir(), rec).unwrap();
    assert!(matches!(
        fx.update().await.unwrap_err(),
        CtlError::Ambiguous(_)
    ));
}

#[tokio::test]
async fn update_args_version_mismatch_with_from_dir_is_refused() {
    let fx = Fixture::new();
    let dir = fx.release(NEW);
    let args = UpdateArgs {
        version: Some(v("1.4.0")),
        from_dir: Some(dir),
        force: false,
        hub_chain: false,
    };
    let err = update_hub(&args, &fx.layout, &fx.runner, &fx.src(), &host())
        .await
        .unwrap_err();
    assert!(err.to_string().starts_with("version_mismatch"), "{err}");
    assert!(fx.events().is_empty());
}

// Compose v5.8.0: `docker compose ps --all --format '{{.State}}' hub` печатает одно
// состояние в строке (без `--all` остановленный контейнер не показан), а не JSON-массив.
fn is_ps_state(cmd: &Cmd) -> bool {
    has(cmd, &["ps", "--all", "--format", "{{.State}}", "hub"])
}

/// Состояние контейнера и его журнал подменяются раньше проверки готовности хаба.
fn failing_candidate(state: &str) -> FakeRunner {
    let runner = FakeRunner::new();
    let state = state.to_owned();
    runner.respond(move |cmd| {
        if is_ps_state(cmd) {
            return Some(Output::ok(state.clone()));
        }
        if has(cmd, &["logs", "--no-color", "--tail", "20", "hub"]) {
            return Some(Output {
                status: 0,
                stdout: Vec::new(),
                stderr: b"migration 0003 failed: relation \"jobs\" already exists\n".to_vec(),
            });
        }
        if has(cmd, &["exec", "-T", "hub"]) && has(cmd, &["probe"]) {
            return Some(Output {
                status: 1,
                stdout: format!("{{\"ready\":false,\"version\":\"{NEW}\"}}\n").into_bytes(),
                stderr: Vec::new(),
            });
        }
        None
    });
    runner
}

#[tokio::test]
async fn probe_stops_when_candidate_container_exited() {
    let fx = Fixture::new();
    let runner = failing_candidate("exited\n");
    let err = probe(&runner, &fx.layout, &host(), &v(NEW))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("exited"), "{text}");
    assert!(text.contains("migration 0003 failed"), "{text}");
    assert_eq!(
        runner.calls().iter().filter(|c| is_ps_state(c)).count(),
        1,
        "must stop after one state check"
    );
    assert!(
        !runner
            .calls()
            .iter()
            .any(|c| has(c, &["exec", "-T", "hub"]) && has(c, &["probe"])),
        "must not exec probe on a dead container"
    );
}

#[tokio::test]
async fn probe_retries_while_container_restarting() {
    let fx = Fixture::new();
    let runner = failing_candidate("restarting\n");
    let err = probe(&runner, &fx.layout, &host(), &v(NEW))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("hub probe exited with 1"), "{text}");
    assert!(!text.contains("container is restarting"), "{text}");
    assert_eq!(
        runner.calls().iter().filter(|c| is_ps_state(c)).count(),
        host().retries as usize,
        "a transient restart must not cut the retry loop short"
    );
}

#[tokio::test]
async fn probe_retries_when_container_is_not_listed() {
    let fx = Fixture::new();
    let runner = failing_candidate("");
    let err = probe(&runner, &fx.layout, &host(), &v(NEW))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("hub probe exited with 1"), "{text}");
    assert_eq!(
        runner.calls().iter().filter(|c| is_ps_state(c)).count(),
        host().retries as usize,
        "an empty ps output means no container yet, not a failure verdict"
    );
}

#[tokio::test]
async fn probe_failure_reason_includes_candidate_log_tail() {
    let fx = Fixture::new();
    fx.world().fail.insert("candidate");
    fx.world().hub = Some(NEW.into());
    let runner = failing_candidate("running\n");
    let err = probe(&runner, &fx.layout, &host(), &v(NEW))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("hub probe exited with 1"), "{text}");
    assert!(
        text.contains("migration 0003 failed"),
        "the failed migration must be named: {text}"
    );
}
