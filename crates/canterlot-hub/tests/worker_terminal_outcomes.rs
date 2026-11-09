//! Каждый конечный исход, который воркер пишет фактом `ExecutionState`, допустим в таблице
//! переходов хаба. Отвергнутый факт оставил бы исполнение живым навсегда: сессию нельзя
//! ни запустить, ни убрать в корзину. Второй тест читает исходники воркера и требует,
//! чтобы новый исход попал в таблицу этого теста.

use std::collections::BTreeSet;

use canterlot_hub::{executions, model::ExecutionState, testkit};
use sqlx::PgPool;
use uuid::Uuid;

use ExecutionState::{Created, Failed, OwnerLost, Running, Starting, Stopped, Stopping};

/// (путь до конечного факта, конечное состояние, исход) — как их пишет воркер.
const WORKER_TERMINALS: &[(&[ExecutionState], ExecutionState, &str)] = &[
    // supervisor/run.rs `finish_stop` и supervisor/mod.rs `exit_outcome`: после `stopping`.
    (&[Starting, Stopping], Stopped, "user_stop"),
    (&[Starting, Stopping], Stopped, "force_stopped"),
    (&[Starting, Stopping], Stopped, "start_cancelled"),
    (&[Starting, Stopping], Stopped, "normal_exit"),
    (&[Starting, Stopping], Failed, "start_failed"),
    (&[Starting, Stopping], Failed, "codex_state_backfill_stuck"),
    (&[Starting, Stopping], Failed, "invalid_instructions"),
    (&[Starting, Stopping], Failed, "auth_required"),
    (&[Starting, Stopping], Failed, "cli_incompatible"),
    (&[Starting, Stopping], Failed, "context_exhausted"),
    (&[Starting, Stopping], Failed, "protocol_incompatible"),
    (&[Starting, Stopping], Failed, "crashed"),
    // exec/manager.rs: отмена до запуска сопровождения.
    (&[], Stopped, "start_cancelled"),
    // exec/lost.rs: исполнение без сопровождения.
    (&[Starting, Running, OwnerLost], Failed, "supervisor_died"),
    (&[Starting, Running, OwnerLost], Failed, "admin_resolved"),
    (&[Starting, Running, OwnerLost], Stopped, "machine_rebooted"),
    (&[Starting, Running], Stopped, "machine_rebooted"),
];

#[sqlx::test(migrator = "canterlot_hub::db::MIGRATOR")]
async fn every_worker_terminal_outcome_is_accepted(pool: PgPool) {
    let seed = testkit::seed(&pool).await;
    for (path, terminal, outcome) in WORKER_TERMINALS {
        let conversation = testkit::seed_conversation(&pool, seed.owner_id, seed.machine_id).await;
        let started = executions::start(&pool, seed.owner_id, conversation, Uuid::now_v7())
            .await
            .unwrap();
        for next in *path {
            executions::set_state(&pool, started.execution_id, *next, None, Uuid::now_v7())
                .await
                .unwrap_or_else(|error| panic!("{outcome}: path to {next:?}: {error:?}"));
        }
        let result = executions::set_state(
            &pool,
            started.execution_id,
            *terminal,
            Some(outcome),
            Uuid::now_v7(),
        )
        .await;
        assert!(
            result.is_ok(),
            "{:?} -> {terminal:?}({outcome}) rejected: {result:?}",
            path.last().unwrap_or(&Created)
        );
    }
}

fn literals(source: &str) -> Vec<&str> {
    source.split('"').skip(1).step_by(2).collect()
}

fn between<'a>(source: &'a str, start: &str, end: &str) -> &'a str {
    let from = source
        .find(start)
        .unwrap_or_else(|| panic!("`{start}` not found"));
    let rest = &source[from..];
    &rest[..rest
        .find(end)
        .unwrap_or_else(|| panic!("`{end}` not found"))]
}

fn outcome_fields(source: &str) -> impl Iterator<Item = &str> {
    source.lines().filter_map(|line| {
        let rest = line.trim().strip_prefix("outcome: \"")?;
        rest.split('"').next()
    })
}

#[test]
fn worker_sources_list_no_unknown_terminal_outcome() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../canterlot-worker/src/");
    let read = |path: &str| std::fs::read_to_string(format!("{root}{path}")).unwrap();
    let run = read("supervisor/run.rs");
    let module = read("supervisor/mod.rs");
    let mut found = BTreeSet::new();
    let skip = ["stopped", "failed", "process_exited"];
    for literal in literals(between(&run, "fn finish_stop", "self.close_blocks(reason)"))
        .into_iter()
        .chain(literals(between(&module, "pub fn exit_outcome", "\n}\n")))
    {
        if !skip.contains(&literal) {
            found.insert(literal.to_owned());
        }
    }
    for path in ["exec/lost.rs", "exec/manager.rs"] {
        found.extend(outcome_fields(&read(path)).map(str::to_owned));
    }
    assert!(found.contains("invalid_instructions"), "{found:?}");
    let listed: BTreeSet<_> = WORKER_TERMINALS
        .iter()
        .map(|(_, _, outcome)| (*outcome).to_owned())
        .collect();
    let missing: Vec<_> = found.difference(&listed).collect();
    assert!(
        missing.is_empty(),
        "worker outcomes missing from WORKER_TERMINALS (and maybe from legal_transition): {missing:?}"
    );
}
