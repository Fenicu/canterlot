//! Тесты разбора на реальных кадрах Claude Code 2.1.310 (выдержки из доказательств).

use serde_json::Value;

use crate::config::check_isolation;
use crate::correlate::{Delivery, InputLedger, ToolLedger};
use crate::framing::{Frame, LineFramer, decode_line};
use crate::protocol::{self, Kind, ToolInputAssembler};

const START_STREAM: &str = include_str!("../tests/fixtures/start_stream.jsonl");
const TOOLS: &str = include_str!("../tests/fixtures/tools.jsonl");
const INTERRUPT: &str = include_str!("../tests/fixtures/interrupt.jsonl");
const SERVICE_FAILURE: &str = include_str!("../tests/fixtures/service_failure.jsonl");

fn frames(text: &str) -> Vec<Value> {
    text.lines()
        .map(|l| serde_json::from_str(l).expect("fixture line is json"))
        .collect()
}

#[test]
fn real_stream_survives_any_chunking() {
    let all = [START_STREAM, TOOLS, INTERRUPT, SERVICE_FAILURE].concat();
    let expected: Vec<&str> = all.lines().collect();
    for chunk in [1usize, 2, 3, 7, 13, 4096] {
        let mut framer = LineFramer::new(1 << 20);
        let mut got = Vec::new();
        for part in all.as_bytes().chunks(chunk) {
            for f in framer.push(part) {
                let Frame::Line(b) = f else {
                    panic!("oversized")
                };
                got.push(decode_line(&b).expect("utf-8"));
            }
        }
        assert_eq!(got, expected, "chunk size {chunk}");
    }
}

#[test]
fn start_stream_sample_classifies_and_correlates() {
    let fs = frames(START_STREAM);
    let init = fs
        .iter()
        .find(|v| protocol::classify(v) == Kind::SystemInit)
        .expect("init");
    let (isolated, details) = check_isolation(init, &[]);
    assert!(isolated, "{details}");
    let replay = fs.iter().find(|v| protocol::is_replay(v)).expect("replay");
    let our_uuid = protocol::uuid(replay).expect("uuid").to_owned();
    assert!(
        fs.iter()
            .any(|v| protocol::classify(v) == Kind::Other("command_lifecycle".into()))
    );

    let mut ledger = InputLedger::default();
    ledger.written(&our_uuid, 0);
    for (i, v) in fs.iter().enumerate() {
        ledger.observe(i as u64 + 1, v);
    }
    let entry = ledger.get(&our_uuid).expect("entry");
    assert_eq!(entry.delivery(), Delivery::Answered);
    assert!(entry.echo_ms.is_some() && entry.first_reply_ms.is_some());

    let deltas: String = fs.iter().filter_map(protocol::text_delta).collect();
    let assistant = fs
        .iter()
        .find(|v| protocol::classify(v) == Kind::Assistant)
        .expect("assistant");
    assert_eq!(deltas, protocol::assistant_text(assistant));
    let result = fs
        .iter()
        .find(|v| protocol::classify(v) == Kind::Result)
        .expect("result");
    assert_eq!(protocol::session_id(result), protocol::session_id(init));
    assert_eq!(result["subtype"], "success");
    assert!(protocol::answered_uuids(result).contains(&our_uuid));
}

#[test]
fn tools_sample_links_results_and_assembles_inputs() {
    let fs = frames(TOOLS);
    let mut tools = ToolLedger::default();
    let mut asm = ToolInputAssembler::default();
    for v in &fs {
        tools.observe(v);
        asm.observe(v);
    }
    assert!(tools.unmatched_uses().is_empty());
    assert!(tools.orphan_or_duplicate_results().is_empty());
    let read = tools
        .uses()
        .iter()
        .find(|u| u.name == "Read")
        .expect("Read");
    assert!(tools.result_for(&read.id).expect("read result").is_error);
    let bash = tools
        .uses()
        .iter()
        .find(|u| u.name == "Bash")
        .expect("Bash");
    assert!(!tools.result_for(&bash.id).expect("bash result").is_error);
    assert!(!asm.done.is_empty());
    for a in &asm.done {
        let u = tools
            .uses()
            .iter()
            .find(|u| u.id == a.tool_use_id)
            .expect("use");
        assert_eq!(a.input.as_ref().expect("complete json"), &u.input);
    }
    assert_eq!(asm.pending(), 0);
}

#[test]
fn interrupt_sample_has_receipt_and_aborted_result() {
    let fs = frames(INTERRUPT);
    let receipt = fs
        .iter()
        .find(|v| {
            protocol::classify(v) == Kind::ControlResponse
                && v["response"]["response"]["cancelled"]
                    .as_array()
                    .is_some_and(|c| !c.is_empty())
        })
        .expect("receipt with cancelled");
    assert_eq!(receipt["response"]["subtype"], "success");
    assert!(receipt["response"]["request_id"].as_str().is_some());
    assert!(fs.iter().any(|v| v["terminal_reason"] == "aborted_tools"
        && v["subtype"] == "error_during_execution"
        && v["is_error"] == true));
}

#[test]
fn service_failure_sample_surfaces_retries_and_errors() {
    let fs = frames(SERVICE_FAILURE);
    let attempts: Vec<u64> = fs
        .iter()
        .filter(|v| protocol::classify(v) == Kind::System("api_retry".into()))
        .filter_map(|v| v["attempt"].as_u64())
        .collect();
    assert!(attempts.len() >= 2);
    assert!(fs.iter().any(|v| v["error"] == "model_not_found"));
    let errors = fs
        .iter()
        .filter(|v| protocol::classify(v) == Kind::Result && v["is_error"] == true)
        .count();
    assert!(errors >= 2);
}
