//! Сценарии этапа 0: каждый пишет свой файл evidence и заканчивается вердиктом.

mod approvals;
mod basic;
mod control;
mod diagnostics;
mod failure;

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};

use crate::evidence::Recorder;
use crate::isolation;

pub struct Verdict {
    pub verdict: &'static str,
    pub reason: String,
}

impl Verdict {
    pub fn new(verdict: &'static str, reason: impl Into<String>) -> Self {
        Self {
            verdict,
            reason: reason.into(),
        }
    }
}

pub struct Scenario {
    pub name: &'static str,
    /// Сколько запросов turn/start и turn/steer отправляет сценарий.
    pub model_turns_planned: u32,
}

pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "diagnostics",
        model_turns_planned: 0,
    },
    Scenario {
        name: "lifecycle",
        model_turns_planned: 0,
    },
    Scenario {
        name: "start_stream",
        model_turns_planned: 1,
    },
    Scenario {
        name: "multi_input",
        model_turns_planned: 6,
    },
    Scenario {
        name: "delivery_ack",
        model_turns_planned: 2,
    },
    Scenario {
        name: "tools",
        model_turns_planned: 1,
    },
    Scenario {
        name: "permissions",
        model_turns_planned: 1,
    },
    Scenario {
        name: "question",
        model_turns_planned: 3,
    },
    Scenario {
        name: "interrupt",
        model_turns_planned: 3,
    },
    Scenario {
        name: "resume",
        model_turns_planned: 4,
    },
    Scenario {
        name: "service_failure",
        model_turns_planned: 2,
    },
    Scenario {
        name: "question_default_mode",
        model_turns_planned: 1,
    },
    Scenario {
        name: "interrupt_cleanup",
        model_turns_planned: 1,
    },
    Scenario {
        name: "image_input",
        model_turns_planned: 1,
    },
    Scenario {
        name: "background",
        model_turns_planned: 1,
    },
];

pub fn evidence_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/stage0/evidence/codex")
}

pub async fn run(name: &str) -> anyhow::Result<Verdict> {
    let path = evidence_dir().join(format!("{name}.jsonl"));
    let rec = Recorder::create(&path)?;
    rec.note(
        json!({ "scenario": name, "probe_version": env!("CARGO_PKG_VERSION"),
        "model": isolation::MODEL, "effort": isolation::EFFORT }),
    );
    let scratch = isolation::fresh_scratch(name)?;
    rec.note(json!({ "scratch": scratch.to_string_lossy() }));
    let result = match name {
        "diagnostics" => diagnostics::run(&rec, &scratch).await,
        "lifecycle" => control::lifecycle(&rec, &scratch).await,
        "start_stream" => basic::start_stream(&rec, &scratch).await,
        "multi_input" => basic::multi_input(&rec, &scratch).await,
        "delivery_ack" => basic::delivery_ack(&rec, &scratch).await,
        "tools" => basic::tools(&rec, &scratch).await,
        "permissions" => approvals::permissions(&rec, &scratch).await,
        "question" => approvals::question(&rec, &scratch).await,
        "interrupt" => control::interrupt(&rec, &scratch).await,
        "resume" => control::resume(&rec, &scratch).await,
        "service_failure" => failure::service_failure(&rec, &scratch).await,
        "background" => control::background(&rec, &scratch).await,
        "image_input" => basic::image_input(&rec, &scratch).await,
        "question_default_mode" => approvals::question_default_mode(&rec, &scratch).await,
        "interrupt_cleanup" => control::interrupt_cleanup(&rec, &scratch).await,
        other => anyhow::bail!("unknown scenario {other}"),
    };
    let verdict = match result {
        Ok(v) => v,
        Err(e) => {
            rec.note(json!({ "scenario_error": format!("{e:#}") }));
            Verdict::new("FAIL", format!("probe error: {e:#}"))
        }
    };
    let (starts, steers) = rec.turn_counts();
    rec.note(json!({ "model_requests": { "turn_start": starts, "turn_steer": steers } }));
    rec.verdict(verdict.verdict, &verdict.reason);
    Ok(verdict)
}

/// Запуск вспомогательной команды CLI с очищенным окружением.
pub async fn run_cli(args: &[&str]) -> anyhow::Result<(Option<i32>, String, String)> {
    let out = tokio::process::Command::new(isolation::CODEX_BIN)
        .args(args)
        .env_clear()
        .envs(isolation::child_env())
        .output()
        .await?;
    Ok((
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

pub fn has_word(text: &str, word: &str) -> bool {
    text.to_lowercase().contains(&word.to_lowercase())
}

pub const TURN_WAIT: Duration = Duration::from_secs(180);

pub fn resume_params(thread_id: &str, scratch: &std::path::Path) -> Value {
    json!({
        "threadId": thread_id,
        "excludeTurns": true,
        "cwd": scratch.to_string_lossy(),
        "model": isolation::MODEL,
        "approvalPolicy": isolation::APPROVAL_POLICY,
        "approvalsReviewer": "user",
        "sandbox": isolation::SANDBOX_MODE,
    })
}
