#![forbid(unsafe_code)]

//! Review findings for the Codex adapter (not committed): each test reproduces
//! one defect and is expected to FAIL on the current code.

use canterlot_worker::{
    adapter::{
        self, Adapter, AdapterContext, AdapterEvent, CloseMode, LimitFact, LimitWindow, StartSpec,
        Transmission, TransmitOutcome,
        codex::limits::{CrunchContext, LimitTracker},
    },
    config::CodexConfig,
    containment::TreeContainment,
};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tempfile::TempDir;
use uuid::Uuid;

use crate::fake_exec;

const WAIT: Duration = Duration::from_secs(15);

const REPLAY: &str = r#"#!/usr/bin/env python3
import json, sys, pathlib, time
d = pathlib.Path(__file__).parent
if '--version' in sys.argv:
    print('codex-cli 0.164.0'); sys.exit(0)
log = (d/'stdin.log').open('a')
def fx(method):
    p = d/'fixtures'/(method.replace('/','_')+'.json')
    return json.loads(p.read_text()) if p.exists() else None
lines = []
for raw in (d/'evidence.jsonl').read_text().splitlines():
    raw = raw.strip()
    if raw: lines.append(json.loads(raw))
def data(w): return w.get('data')
def is_in_method(w):
    return w.get('dir')=='in' and isinstance(data(w),dict) and 'method' in data(w)
def omitted(dd):
    return isinstance(dd,dict) and isinstance(dd.get('result'),str) and dd['result'].startswith('<omitted')
idmap = {}; clients = {}; cur = ''; pos = 0
def out(w):
    dd = data(w)
    if w.get('dir')=='sleep':
        time.sleep(dd/1000); return
    if isinstance(dd,str):
        if dd == '@exit': sys.exit(0)
        return
    if isinstance(dd,dict) and '__raw__' in dd:
        print(dd['__raw__'],flush=True); return
    if w.get('dir')=='err':
        sys.stderr.write(dd if isinstance(dd,str) else json.dumps(dd)); sys.stderr.write('\n'); sys.stderr.flush(); return
    if w.get('dir')!='out': return
    dd = json.loads(json.dumps(dd))
    if omitted(dd):
        rid = dd.get('id'); aid = idmap.get(json.dumps(rid,sort_keys=True),rid)
        f = fx(cur)
        print(json.dumps({'id':aid,'result':f if f is not None else {}}),flush=True); return
    if 'id' in dd and 'method' not in dd:
        k = json.dumps(dd['id'],sort_keys=True)
        if k in idmap: dd['id'] = idmap[k]
    p = dd.get('params')
    if isinstance(p,dict):
        it = p.get('item')
        if isinstance(it,dict) and it.get('type')=='userMessage':
            c = clients.get(p.get('threadId'))
            if c is not None: it['clientId'] = c
    print(json.dumps(dd),flush=True)
def flush():
    global pos
    while pos < len(lines) and not is_in_method(lines[pos]):
        w = lines[pos]
        if w.get('dir') != 'in': out(w)
        pos += 1
for raw in sys.stdin:
    log.write(raw); log.flush()
    try: msg = json.loads(raw)
    except: continue
    m = msg.get('method') if isinstance(msg,dict) else None
    if not isinstance(m,str):
        flush(); continue
    if m in ('turn/start', 'turn/steer'):
        p = msg.get('params') or {}
        if p.get('threadId') and p.get('clientUserMessageId'):
            clients[p['threadId']] = p['clientUserMessageId']
    flush()
    j = pos
    while j < len(lines):
        w = lines[j]
        if is_in_method(w) and data(w).get('method') == m: break
        j += 1
    if j < len(lines):
        rec = data(lines[j])
        if 'id' in msg and isinstance(rec,dict) and 'id' in rec:
            idmap[json.dumps(rec['id'],sort_keys=True)] = msg['id']
        cur = m; pos = j+1; flush()
    elif m != 'initialized' and 'id' in msg:
        f = fx(m)
        print(json.dumps({'id':msg['id'],'result':f if f is not None else {}}),flush=True)
"#;

fn w(dir: &str, data: Value) -> String {
    serde_json::to_string(&json!({"dir": dir, "data": data})).unwrap()
}
fn w_in(id: i64, method: &str, params: Value) -> String {
    w("in", json!({"id": id, "method": method, "params": params}))
}
fn w_out(data: Value) -> String {
    w("out", data)
}
fn note(method: &str, params: Value) -> String {
    w_out(json!({"method": method, "params": params}))
}
fn handshake(thread: &str) -> Vec<String> {
    vec![
        w_in(0, "initialize", json!({})),
        w_out(json!({"id": 0, "result": {"codexHome": "/h"}})),
        w("in", json!({"method": "initialized", "params": {}})),
        w_in(1, "account/read", json!({})),
        w_out(json!({"id": 1, "result": {"account": {"type": "chatgpt"}}})),
        w_in(2, "config/read", json!({})),
        w_out(json!({"id": 2, "result": "<omitted: test>"})),
        w_in(3, "thread/start", json!({})),
        w_out(json!({"id": 3, "result": {
            "approvalPolicy": "on-request", "model": "gpt-6-luna",
            "sandbox": {"networkAccess": false, "type": "readOnly"},
            "instructionSources": [],
            "thread": {"id": thread, "model": "gpt-6-luna", "cliVersion": "0.164.0"}}})),
    ]
}
fn turn_started(thread: &str, turn: &str) -> String {
    note(
        "turn/started",
        json!({"threadId": thread, "turn": {"id": turn, "status": "inProgress", "items": []}}),
    )
}
fn user_message(thread: &str, turn: &str) -> Vec<String> {
    let item =
        json!({"id": format!("u-{turn}"), "type": "userMessage", "clientId": "x", "content": []});
    vec![
        note(
            "item/started",
            json!({"threadId": thread, "turnId": turn, "item": item}),
        ),
        note(
            "item/completed",
            json!({"threadId": thread, "turnId": turn, "completedAtMs": 1, "item": item}),
        ),
    ]
}
fn turn_end(thread: &str, turn: &str) -> String {
    note(
        "turn/completed",
        json!({"threadId": thread, "turn": {"id": turn, "status": "completed", "items": []}}),
    )
}

struct Fake {
    dir: TempDir,
    group: TreeContainment,
    adapter: Box<dyn Adapter>,
    spec: StartSpec,
}

impl Fake {
    fn new(lines: Vec<String>, deadline: Duration) -> Self {
        let dir = TempDir::new().unwrap();
        let exe = dir.path().join("fake-codex");
        fake_exec::write_executable(&exe, REPLAY, 0o700);
        std::fs::write(dir.path().join("evidence.jsonl"), lines.join("\n") + "\n").unwrap();
        std::os::unix::fs::symlink(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/it/fixtures/codex"),
            dir.path().join("fixtures"),
        )
        .unwrap();
        let context = AdapterContext {
            transmit_deadline: deadline,
            ready_deadline: deadline,
            exec_dir: dir.path().join("exec"),
            claude_code: None,
            codex: Some(CodexConfig {
                executable: exe,
                ..Default::default()
            }),
            opencode: None,
            antigravity: None,
        };
        let adapter = adapter::registry_with("codex", &context).unwrap();
        let spec = StartSpec {
            execution_id: Uuid::now_v7(),
            generation: 1,
            workdir: dir.path().into(),
            native_conversation_id: None,
            profile: "codex".into(),
            model: "gpt-6-luna".into(),
            effort: "low".into(),
            permission_mode: "ask".into(),
            instructions: canterlot_worker::adapter::InstructionSpec {
                mode: canterlot_worker::adapter::InstructionMode::Owner,
                text: String::new(),
                preset_id: None,
                revision: 0,
                title: String::new(),
            },
            isolate_mcp: false,
        };
        Self {
            dir,
            group: TreeContainment::new().unwrap(),
            adapter,
            spec,
        }
    }
    fn tx(&self, text: &str) -> Transmission {
        Transmission {
            command_id: Uuid::now_v7(),
            attempt: 1,
            text: text.into(),
            files: vec![],
            settings: None,
        }
    }
    async fn run_to_end(&mut self, t: Transmission) -> TransmitOutcome {
        let out = tokio::time::timeout(WAIT, self.adapter.transmit(&t, &self.group))
            .await
            .unwrap();
        loop {
            let event = tokio::time::timeout(WAIT, self.adapter.next_event())
                .await
                .expect("event");
            if matches!(event, AdapterEvent::RunFinished { input_command_id, .. } if input_command_id == t.command_id)
            {
                break;
            }
        }
        out
    }
    fn turn_starts(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("stdin.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|m| m["method"] == "turn/start")
            .map(|m| m["params"].clone())
            .collect()
    }
}

/// Finding: a turn adopted from `turn/started` that ends before the `turn/start`
/// response is parsed drops the input's settings (`settle_late_turn` clears
/// `pending_turn` in the `adopted_turn_ended` branch without
/// `apply_settings_override`). The server ran the turn with effort=high, yet the
/// next plain turn re-sends the stale effort=low and silently reverts it.
#[tokio::test]
async fn adopted_turn_without_response_keeps_effort_change() {
    let mut lines = handshake("th");
    lines.push(w_in(10, "turn/start", json!({})));
    lines.push(turn_started("th", "turn-1"));
    lines.extend(user_message("th", "turn-1"));
    lines.push(turn_end("th", "turn-1"));
    lines.push(w_in(11, "turn/start", json!({})));
    lines.push(w_out(
        json!({"id": 11, "result": {"turn": {"id": "turn-2", "status": "inProgress"}}}),
    ));
    lines.extend(user_message("th", "turn-2"));
    lines.push(turn_end("th", "turn-2"));
    let mut fake = Fake::new(lines, Duration::from_millis(300));
    tokio::time::timeout(WAIT, fake.adapter.start(&fake.spec, &fake.group))
        .await
        .unwrap()
        .unwrap();
    let mut first = fake.tx("first");
    first.settings = Some(adapter::SettingsChange {
        effort: Some("high".into()),
        ..Default::default()
    });
    fake.run_to_end(first).await;
    let second = fake.tx("second");
    fake.run_to_end(second).await;
    let starts = fake.turn_starts();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0]["effort"], "high");
    assert_eq!(
        starts[1]["effort"], "high",
        "effort change reverted: {}",
        starts[1]
    );
    fake.adapter
        .close(CloseMode::Graceful, &fake.group)
        .await
        .unwrap();
}

/// Finding: `on_fresh_read` for `usageLimitExceeded` reports `Reached` with a
/// reset time already in the past when the fresh read carries no usable window
/// (e.g. only another `limitId`); `on_read_failed` filters such resets precisely
/// because a past reset wakes the queue at once.
#[test]
fn fresh_read_without_windows_does_not_report_a_past_reset() {
    let now = 2_000_000_000_000u64;
    let mut tracker = LimitTracker::default();
    tracker.on_snapshot(&json!({
        "limitId": "codex",
        "primary": {"usedPercent": 40.0, "windowDurationMins": 300, "resetsAt": (now / 1000) - 3600},
    }));
    let facts = tracker.on_fresh_read(
        &json!({"rateLimits": {"limitId": "codex_other", "primary": null, "secondary": null}}),
        CrunchContext::UsageLimitExceeded,
        now,
    );
    let reached = facts
        .iter()
        .find_map(|f| match f {
            LimitFact::Reached {
                window,
                resets_at_ms,
            } => Some((*window, *resets_at_ms)),
            _ => None,
        })
        .expect("Reached");
    assert_eq!(reached.0, LimitWindow::FiveHour);
    assert!(
        reached.1.is_none_or(|reset| reset > now),
        "Reached carries a past reset {:?} (now {now})",
        reached.1
    );
}
