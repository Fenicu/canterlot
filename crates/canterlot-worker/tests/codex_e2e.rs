#![forbid(unsafe_code)]

//! Сквозные тесты Codex (задача 7 плана 2b). Поддельный app-server — скрипт
//! python: ждёт методы адаптера, пишет кадры свидетельства с перезаписями id.
//! Настоящий `codex` не запускается (кроме отметки live smoke владельцем).

use canterlot_proto::requests::{DecisionPayload, PermissionBehavior};
use canterlot_worker::{
    adapter::{
        self, Adapter, AdapterContext, AdapterEvent, BlockKind, CloseMode, StartSpec, Transmission,
        TransmitOutcome,
        codex::{CodexCatalog, input::model_entries},
    },
    config::CodexConfig,
    containment::TreeContainment,
};

use canterlot_worker::adapter::codex::home::TempDirPolicy;
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};
use tempfile::TempDir;
use uuid::Uuid;

#[path = "support/fake_exec.rs"]
mod fake_exec;

const WAIT: Duration = Duration::from_secs(15);

// Только локальные трубы. Скрипт ждёт строки stdin с `method`, иначе считает
// ответом: отдаёт накопленное до следующей входящей строки с методом.
// Перезаписи: id ответов, clientId userMessage за ходом, omitted → фикстура,
// err → stderr, неизвестный метод → `{}` (или фикстура), `@exit` → выход,
// `__raw__` → сырая строка, `dir: sleep` → пауза на `data` мс.
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
    if m == 'turn/start':
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

fn repo_fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex")
}

fn write_lines(dir: &TempDir, name: &str, lines: &[String]) {
    std::fs::write(dir.path().join(name), lines.join("\n") + "\n").unwrap();
}

fn evidence_slice(name: &str, from: usize, to: usize) -> Vec<String> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/stage0/evidence/codex")
        .join(format!("{name}.jsonl"));
    std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .enumerate()
        .filter(|(index, _)| (from..=to).contains(&(index + 1)))
        .map(|(_, line)| line.to_owned())
        .collect()
}

fn w(dir: &str, data: Value) -> String {
    serde_json::to_string(&json!({"dir": dir, "data": data})).unwrap()
}

fn w_in(id: i64, method: &str, params: Value) -> String {
    w("in", json!({"id": id, "method": method, "params": params}))
}

fn w_in_notification(method: &str) -> String {
    w("in", json!({"method": method, "params": {}}))
}

fn w_out(data: Value) -> String {
    w("out", data)
}

fn thread_result(thread: &str, model: &str) -> Value {
    json!({
        "approvalPolicy": "on-request",
        "model": model,
        "sandbox": {"networkAccess": false, "type": "readOnly"},
        "instructionSources": [],
        "thread": {"id": thread, "model": model, "cliVersion": "0.164.0"},
    })
}

fn handshake(thread: &str) -> Vec<String> {
    vec![
        w_in(0, "initialize", json!({"clientInfo": {"name": "t"}})),
        w_out(json!({"id": 0, "result": {"codexHome": "/h"}})),
        w_in_notification("initialized"),
        w_in(1, "account/read", json!({"refreshToken": false})),
        w_out(
            json!({"id": 1, "result": {"account": {"type": "chatgpt"}, "requiresOpenaiAuth": true}}),
        ),
        w_in(2, "config/read", json!({})),
        w_out(json!({"id": 2, "result": "<omitted: test>"})),
        w_in(3, "thread/start", json!({})),
        w_out(json!({"id": 3, "result": thread_result(thread, "gpt-6-luna")})),
    ]
}

fn turn_exchange(request_id: i64, thread: &str, turn_id: &str) -> Vec<String> {
    vec![
        w_in(request_id, "turn/start", json!({})),
        w_out(
            json!({"id": request_id, "result": {"turn": {"id": turn_id, "status": "inProgress"}}}),
        ),
        w_out(json!({
            "method": "item/started",
            "params": {"threadId": thread, "turnId": turn_id,
                "item": {"id": "u-mini", "type": "userMessage", "clientId": "mini", "content": []}},
        })),
        w_out(json!({
            "method": "item/completed",
            "params": {"threadId": thread, "turnId": turn_id, "completedAtMs": 1,
                "item": {"id": "u-mini", "type": "userMessage", "clientId": "mini", "content": []}},
        })),
    ]
}

struct Fake {
    dir: TempDir,
    group: TreeContainment,
    adapter: Box<dyn Adapter>,
    spec: StartSpec,
}

impl Fake {
    fn new(evidence: Vec<String>) -> Self {
        Self::with_deadline(evidence, WAIT)
    }

    fn with_deadline(evidence: Vec<String>, transmit_deadline: Duration) -> Self {
        let dir = TempDir::new().unwrap();
        let exe = dir.path().join("fake-codex");
        fake_exec::write_executable(&exe, REPLAY, 0o700);
        write_lines(&dir, "evidence.jsonl", &evidence);
        std::os::unix::fs::symlink(repo_fixtures(), dir.path().join("fixtures")).unwrap();
        let context = AdapterContext {
            transmit_deadline,
            exec_dir: dir.path().join("exec"),
            claude_code: None,
            codex: Some(CodexConfig {
                executable: exe,
                ..Default::default()
            }),
            opencode: None,
        };
        let adapter = adapter::registry_with("codex", &context).expect("Codex registered");
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
        };
        Self {
            dir,
            group: TreeContainment::new().unwrap(),
            adapter,
            spec,
        }
    }

    async fn start(&mut self) -> String {
        tokio::time::timeout(WAIT, self.adapter.start(&self.spec, &self.group))
            .await
            .unwrap()
            .unwrap()
            .native_conversation_id
    }

    async fn event(&mut self) -> AdapterEvent {
        tokio::time::timeout(WAIT, self.adapter.next_event())
            .await
            .expect("adapter event timeout")
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

    async fn send(&mut self, text: &str) -> (Uuid, Vec<AdapterEvent>) {
        let id = self.start_turn(text).await;
        (id, self.finish(id).await)
    }

    async fn start_turn(&mut self, text: &str) -> Uuid {
        let t = self.tx(text);
        let id = t.command_id;
        match tokio::time::timeout(WAIT, self.adapter.transmit(&t, &self.group))
            .await
            .unwrap()
        {
            TransmitOutcome::Accepted(proof) => {
                assert_eq!(proof.command_id, id);
                assert_eq!(proof.kind, "codex_user_message_item");
            }
            other => panic!("delivery: {other:?}"),
        }
        id
    }

    async fn finish(&mut self, id: Uuid) -> Vec<AdapterEvent> {
        let mut events = vec![];
        loop {
            let event = self.event().await;
            let done = matches!(
                event,
                AdapterEvent::RunFinished {
                    input_command_id,
                    ..
                } if input_command_id == id
            );
            events.push(event);
            if done {
                return events;
            }
        }
    }

    async fn decide(&mut self, request_id: &str, decision: &DecisionPayload) -> Value {
        use canterlot_worker::adapter::DecisionOutcome;
        match tokio::time::timeout(WAIT, self.adapter.deliver_decision(request_id, decision))
            .await
            .unwrap()
        {
            DecisionOutcome::Applied(proof) => {
                assert_eq!(proof.proof_kind, "codex_server_request_resolved");
                serde_json::to_value(&proof.payload).unwrap()
            }
            other => panic!("decision: {other:?}"),
        }
    }

    async fn close(&mut self) {
        tokio::time::timeout(WAIT, self.adapter.close(CloseMode::Graceful, &self.group))
            .await
            .unwrap()
            .unwrap();
    }

    fn stdin_log(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("stdin.log")).unwrap_or_default()
    }

    // Ответ адаптера на запрос сервера `id` и события, накопленные за ожиданием.
    async fn reply_to(&mut self, id: i64) -> (Value, Vec<AdapterEvent>) {
        let mut events = vec![];
        for _ in 0..200 {
            let reply = self
                .stdin_log()
                .lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .find(|line| line["id"] == id && line.get("method").is_none());
            if let Some(reply) = reply {
                return (reply, events);
            }
            if let Ok(event) =
                tokio::time::timeout(Duration::from_millis(25), self.adapter.next_event()).await
            {
                events.push(event);
            }
        }
        panic!("no reply to {id}: {events:?}");
    }

    fn stdin_methods(&self) -> Vec<String> {
        self.stdin_log()
            .lines()
            .filter_map(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()?
                    .get("method")?
                    .as_str()
                    .map(str::to_owned)
            })
            .collect()
    }

    fn final_text(events: &[AdapterEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                AdapterEvent::BlockFinal { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }
}

fn allow() -> DecisionPayload {
    DecisionPayload::Permission {
        behavior: PermissionBehavior::Allow,
        message: None,
    }
}

fn model_list_catalog() -> CodexCatalog {
    let text = std::fs::read_to_string(repo_fixtures().join("model_list.json")).unwrap();
    let result: Value = serde_json::from_str(&text).unwrap();
    CodexCatalog {
        version: canterlot_worker::adapter::codex::version::CliVersion::Verified("0.164.0".into()),
        models: model_entries(&result),
    }
}

#[test]
fn catalog_lists_codex_models_from_model_list_when_configured() {
    assert!(
        adapter::catalog_with(None, None, None, None)
            .iter()
            .all(|entry| entry.harness != "codex")
    );
    let catalog = model_list_catalog();
    let entries = adapter::catalog_with(None, None, Some(&catalog), None);
    let entry = entries
        .iter()
        .find(|entry| entry.harness == "codex")
        .unwrap();
    assert_eq!(entry.provider, "codex");
    assert_eq!(entry.title, "Codex");
    assert!(entry.images && entry.settings_midsession && entry.settings_midsession_effort);
    assert_eq!(
        entry.permission_modes,
        ["owner", "ask", "bypass", "read_only"]
    );
    let ids: Vec<&str> = entry.models.iter().map(|model| model.id.as_str()).collect();
    assert_eq!(ids[0], "gpt-6.1-sol", "default model first");
    assert!(ids.contains(&"gpt-6-luna"));
    let luna = entry
        .models
        .iter()
        .find(|model| model.id == "gpt-6-luna")
        .unwrap();
    assert_eq!(luna.efforts, ["low", "medium", "high", "xhigh", "max"]);
}

#[test]
fn registry_builds_codex_adapter_from_context() {
    let dir = TempDir::new().unwrap();
    let context = AdapterContext {
        transmit_deadline: WAIT,
        exec_dir: dir.path().into(),
        claude_code: None,
        codex: Some(CodexConfig::default()),
        opencode: None,
    };
    let adapter = adapter::registry_with("codex", &context).expect("Codex registered");
    let capabilities = adapter.capabilities();
    assert!(matches!(
        capabilities.streaming,
        adapter::Support::Supported
    ));
    assert!(matches!(capabilities.resume, adapter::Support::Supported));
    assert!(matches!(
        capabilities.interrupt,
        adapter::Support::Supported
    ));
    assert!(matches!(
        capabilities.approvals,
        adapter::Support::Supported
    ));
    assert!(matches!(
        capabilities.questions,
        adapter::Support::Supported
    ));
    assert!(matches!(
        capabilities.background_work,
        adapter::Support::Supported
    ));
    assert!(capabilities.interrupt_keeps_process);
    assert!(adapter::known_harness("codex"));
    assert!(!adapter::known_harness("unknown"));
    assert!(
        adapter::registry_with(
            "codex",
            &AdapterContext {
                transmit_deadline: WAIT,
                exec_dir: dir.path().into(),
                claude_code: None,
                codex: None,
                opencode: None,
            }
        )
        .is_none()
    );
    let spec = toml::toml! {
        execution_id = (Uuid::now_v7().to_string())
        generation = 1
        conversation_id = (Uuid::now_v7().to_string())
        protocol_version = 1
        codex = { executable = "/usr/bin/codex" }
    };
    let decoded: canterlot_worker::supervisor::SupervisorSpec = spec.try_into().unwrap();
    assert_eq!(
        decoded.codex.unwrap().executable,
        PathBuf::from("/usr/bin/codex")
    );
}

#[tokio::test]
async fn replay_three_turns_end_to_end() {
    let mut fake = Fake::new(evidence_slice("multi_input", 1, 67));
    let native = fake.start().await;
    assert_eq!(native, "01a171c0-91c8-77d2-b6c0-c8cc9258f157");
    for expected in ["OK1", "OK2", "ALPHA"] {
        let (id, events) = fake.send("say it").await;
        assert!(Fake::final_text(&events).contains(expected), "{events:?}");
        let usage: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                AdapterEvent::RunFinished { usage, .. } => Some(usage.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(usage.len(), 1);
        assert!(!usage[0].is_empty(), "per-turn usage for {id}");
        assert!(matches!(
            fake.event().await,
            AdapterEvent::Phase {
                phase: adapter::Phase::AwaitingInput,
                ..
            }
        ));
    }
    // Строгий FIFO: три разных хода по порядку.
    let methods = fake.stdin_methods();
    assert_eq!(methods.iter().filter(|m| *m == "turn/start").count(), 3);
    fake.close().await;
}

#[tokio::test]
async fn replay_permissions_end_to_end() {
    let mut fake = Fake::new(evidence_slice("permissions", 1, 55));
    fake.start().await;
    let id = fake.start_turn("do it").await;
    // Запрос на команду открыт.
    let mut events = vec![];
    let request_id = loop {
        let event = fake.event().await;
        match &event {
            AdapterEvent::RequestOpened {
                request_id, kind, ..
            } if kind == "permission" => break request_id.clone(),
            AdapterEvent::RunFinished { .. } => panic!("request never opened in {events:?}"),
            _ => events.push(event),
        }
    };
    let proof = fake.decide(&request_id, &allow()).await;
    assert_eq!(proof["request_id"], request_id);
    assert_eq!(proof["decision_kind"], "allow");
    // Правка отклонена зондом: финал со статусом, ход продолжается.
    let rest = fake.finish(id).await;
    events.extend(rest);
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::BlockFinal { summary: Some(summary), .. } if summary.contains("declined")
    )));
    assert!(Fake::final_text(&events).contains("DONE"));
    fake.close().await;
}

fn tool_starts(events: &[AdapterEvent]) -> Vec<(Option<String>, Option<String>)> {
    events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockStarted {
                kind: BlockKind::ToolCall,
                tool_name,
                approved_by,
                ..
            } => Some((tool_name.clone(), approved_by.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn owner_decided_commands_carry_no_cli_policy_mark() {
    // Запись: команда и правка файла, обе через запрос владельцу.
    let mut fake = Fake::new(evidence_slice("permissions", 1, 55));
    fake.start().await;
    let id = fake.start_turn("do it").await;
    let mut events = vec![];
    let request_id = loop {
        let event = fake.event().await;
        match &event {
            AdapterEvent::RequestOpened {
                request_id, kind, ..
            } if kind == "permission" => break request_id.clone(),
            _ => events.push(event),
        }
    };
    fake.decide(&request_id, &allow()).await;
    events.extend(fake.finish(id).await);
    let starts = tool_starts(&events);
    assert!(starts.len() >= 2, "{starts:?}");
    assert!(
        starts.iter().all(|(_, mark)| mark.is_none()),
        "approved or declined by the owner, not by CLI policy: {starts:?}"
    );
    fake.close().await;
}

fn note(method: &str, params: Value) -> String {
    w_out(json!({"method": method, "params": params}))
}

fn command_item(id: &str, status: &str, output: Option<&str>) -> Value {
    json!({"id": id, "type": "commandExecution", "command": "/bin/bash -lc 'ls'",
        "commandActions": [{"command": "ls", "type": "unknown"}], "cwd": "/tmp",
        "status": status, "aggregatedOutput": output, "exitCode": output.map(|_| 0)})
}

fn tool_turn(body: Vec<String>) -> Vec<String> {
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.extend(body);
    lines.push(note(
        "turn/completed",
        json!({"threadId": "thread-mini", "turn": {"id": "turn-1", "status": "completed", "items": []}}),
    ));
    lines
}

fn started(item: Value) -> String {
    note(
        "item/started",
        json!({"threadId": "thread-mini", "turnId": "turn-1", "item": item}),
    )
}

fn completed(item: Value) -> String {
    note(
        "item/completed",
        json!({"threadId": "thread-mini", "turnId": "turn-1", "completedAtMs": 1, "item": item}),
    )
}

#[tokio::test]
async fn silent_command_shows_at_once_when_no_cli_mark_is_possible() {
    let mut lines = handshake("thread-mini");
    let policy = json!({"id": 3, "result": {
        "approvalPolicy": "never", "model": "gpt-6-luna",
        "sandbox": {"type": "dangerFullAccess"}, "instructionSources": [],
        "thread": {"id": "thread-mini", "model": "gpt-6-luna", "cliVersion": "0.164.0"}}});
    lines[8] = w_out(policy);
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(started(command_item("exec-1", "inProgress", None)));
    // Команда ещё идёт: завершения нет, а блок уже должен быть виден.
    lines.push(w_in(11, "turn/interrupt", json!({})));
    let mut fake = Fake::new(lines);
    fake.start().await;
    fake.start_turn("run").await;
    let mut seen = vec![];
    let shown = loop {
        match tokio::time::timeout(Duration::from_secs(2), fake.adapter.next_event()).await {
            Ok(event @ AdapterEvent::BlockStarted { .. }) => break Some(event),
            Ok(event) => seen.push(event),
            Err(_) => break None,
        }
    };
    assert!(shown.is_some(), "the block is hidden: {seen:?}");
    fake.close().await;
}

#[tokio::test]
async fn tool_blocks_stream_live_and_only_unrequested_commands_get_the_cli_mark() {
    let mcp = |status: &str| {
        json!({"id": "mcp-1", "type": "mcpToolCall", "server": "s", "tool": "t",
            "arguments": {}, "status": status,
            "result": {"content": [{"type": "text", "text": "r"}]}})
    };
    let image = json!({"id": "img-1", "type": "imageView", "path": "/tmp/pic.png"});
    let message = |text: &str| json!({"id": "a-1", "type": "agentMessage", "text": text});
    let mut fake = Fake::new(tool_turn(vec![
        started(command_item("exec-y", "inProgress", None)),
        started(mcp("inProgress")),
        note(
            "item/commandExecution/outputDelta",
            json!({"threadId": "thread-mini", "turnId": "turn-1", "itemId": "exec-y", "delta": "tick "}),
        ),
        completed(command_item("exec-y", "completed", Some("tick "))),
        completed(mcp("completed")),
        started(image.clone()),
        started(message("")),
        completed(message("DONE")),
        completed(image),
    ]));
    fake.start().await;
    let (_, events) = fake.send("go").await;
    // Каждое событие блока идёт после его BlockStarted: дельты и Media не теряются.
    let mut seen = std::collections::HashSet::new();
    for (index, event) in events.iter().enumerate() {
        match event {
            AdapterEvent::BlockStarted { block_id, .. } => {
                seen.insert(*block_id);
            }
            AdapterEvent::BlockDelta { block_id, .. }
            | AdapterEvent::BlockFinal { block_id, .. }
            | AdapterEvent::Media { block_id, .. } => assert!(
                seen.contains(block_id),
                "event {index} before its block start: {events:?}"
            ),
            _ => {}
        }
    }
    let position = |name: &str| {
        events.iter().position(|event| {
            matches!(event, AdapterEvent::BlockStarted { tool_name: Some(tool), .. } if tool == name)
        })
    };
    let image_at = position("view_image").expect("image block");
    let message_at = events
        .iter()
        .position(|event| {
            matches!(
                event,
                AdapterEvent::BlockStarted {
                    kind: BlockKind::Text,
                    ..
                }
            )
        })
        .expect("message block");
    assert!(image_at < message_at, "image block not delayed: {events:?}");
    let starts = tool_starts(&events);
    for (tool, mark) in &starts {
        let expected = (tool.as_deref() == Some("shell")).then(|| "cli_policy".to_owned());
        assert_eq!(mark, &expected, "{tool:?}: {starts:?}");
    }
    fake.close().await;
}

#[tokio::test]
async fn settings_update_sandbox_policy_reaches_the_cli_mark() {
    let mut fake = Fake::new(tool_turn(vec![
        note(
            "thread/settings/updated",
            json!({"threadId": "thread-mini", "threadSettings": {
                "approvalPolicy": "on-request",
                "sandboxPolicy": {"type": "dangerFullAccess"}}}),
        ),
        started(command_item("exec-z", "inProgress", None)),
        completed(command_item("exec-z", "completed", Some("ok"))),
    ]));
    fake.start().await;
    let (_, events) = fake.send("go").await;
    let starts = tool_starts(&events);
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert_eq!(
        starts[0].1, None,
        "full access is not a CLI-policy approval"
    );
    fake.close().await;
}

#[tokio::test]
async fn child_thread_settings_update_keeps_the_main_thread_policy() {
    let mut fake = Fake::new(tool_turn(vec![
        note(
            "thread/settings/updated",
            json!({"threadId": "thread-child", "threadSettings": {
                "approvalPolicy": "never",
                "sandboxPolicy": {"type": "dangerFullAccess"}}}),
        ),
        started(command_item("exec-c", "inProgress", None)),
        completed(command_item("exec-c", "completed", Some("ok"))),
    ]));
    fake.start().await;
    let (_, events) = fake.send("go").await;
    let starts = tool_starts(&events);
    assert_eq!(starts.len(), 1, "{starts:?}");
    assert_eq!(
        starts[0].1.as_deref(),
        Some("cli_policy"),
        "subagent settings do not replace the main thread policy"
    );
    fake.close().await;
}

#[tokio::test]
async fn replay_question_end_to_end() {
    let mut fake = Fake::new(evidence_slice("question_default_mode", 1, 41));
    fake.start().await;
    let id = fake.start_turn("ask me").await;
    let mut events = vec![];
    let request_id = loop {
        let event = fake.event().await;
        match &event {
            AdapterEvent::RequestOpened {
                request_id, kind, ..
            } if kind == "question" => break request_id.clone(),
            AdapterEvent::RunFinished { .. } => panic!("question never opened"),
            _ => events.push(event),
        }
    };
    let mut answers = std::collections::BTreeMap::new();
    answers.insert(
        "color_preference".to_owned(),
        canterlot_proto::requests::AnswerValue::Single("Blue".to_owned()),
    );
    let proof = fake
        .decide(&request_id, &DecisionPayload::Question { answers })
        .await;
    assert_eq!(proof["decision_kind"], "answer");
    let rest = fake.finish(id).await;
    events.extend(rest);
    assert!(Fake::final_text(&events).contains("BLUE"));
    fake.close().await;
}

#[tokio::test]
async fn replay_interrupt_end_to_end() {
    use canterlot_worker::adapter::InterruptOutcome;
    let mut fake = Fake::new(evidence_slice("interrupt", 1, 102));
    fake.start().await;
    // Фаза A: прерывание бегущего sleep.
    let id_a = fake.start_turn("sleep please").await;
    match tokio::time::timeout(WAIT, fake.adapter.interrupt())
        .await
        .unwrap()
    {
        InterruptOutcome::Acknowledged => {}
        other => panic!("interrupt: {other:?}"),
    }
    let events_a = fake.finish(id_a).await;
    assert!(
        !events_a.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockStarted {
                kind: BlockKind::Error,
                ..
            }
        )),
        "interrupted turn has no error block"
    );
    // Фаза B: прерывание при висящем разрешении, позднее согласие не применяется.
    let id_b = fake.start_turn("touch please").await;
    let request_id = loop {
        let event = fake.event().await;
        match &event {
            AdapterEvent::RequestOpened { request_id, .. } => break request_id.clone(),
            AdapterEvent::RunFinished { .. } => panic!("approval never opened"),
            _ => {}
        }
    };
    match tokio::time::timeout(WAIT, fake.adapter.interrupt())
        .await
        .unwrap()
    {
        InterruptOutcome::Acknowledged => {}
        other => panic!("interrupt: {other:?}"),
    }
    fake.finish(id_b).await;
    use canterlot_worker::adapter::DecisionOutcome;
    match tokio::time::timeout(WAIT, fake.adapter.deliver_decision(&request_id, &allow()))
        .await
        .unwrap()
    {
        DecisionOutcome::NotApplied(reason) => assert_eq!(reason, "request_not_pending"),
        other => panic!("late decision: {other:?}"),
    }
    // Фаза C: следующий ход работает.
    let (_, events_c) = fake.send("after").await;
    assert!(Fake::final_text(&events_c).contains("AFTER"));
    fake.close().await;
}

fn model_page(ids: &[&str], next: Option<&str>) -> Value {
    let data: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({
                "id": id, "model": id, "displayName": id, "description": "d",
                "hidden": false, "isDefault": *id == ids[0] && next.is_some(),
                "defaultReasoningEffort": "low",
                "supportedReasoningEfforts": [
                    {"reasoningEffort": "low", "description": "l"},
                    {"reasoningEffort": "high", "description": "h"},
                ],
                "inputModalities": ["text", "image"],
            })
        })
        .collect();
    json!({"data": data, "nextCursor": next})
}

#[tokio::test]
async fn session_model_list_reads_data_and_follows_next_cursor() {
    let mut lines = handshake("thread-mini");
    let start = lines.len() - 2;
    let pages = vec![
        w_in(3, "model/list", json!({})),
        w_out(json!({"id": 3, "result": model_page(&["gpt-6-luna"], Some("page-2"))})),
        w_in(4, "model/list", json!({})),
        w_out(json!({"id": 4, "result": model_page(&["gpt-6-astra"], None)})),
    ];
    lines.splice(start..start, pages);
    let mut fake = Fake::new(lines);
    fake.start().await;
    let lists: Vec<Value> = fake
        .stdin_log()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|message| message["method"] == "model/list")
        .collect();
    assert_eq!(lists.len(), 2, "both pages read: {lists:?}");
    assert_eq!(lists[1]["params"]["cursor"], "page-2");
    let mut t = fake.tx("hi");
    for (model, effort, ok) in [
        ("gpt-6-luna", "high", true),
        ("gpt-6-astra", "low", true),
        ("gpt-6-astra", "ultra", false),
        ("no-such-model", "low", false),
    ] {
        t.settings = Some(adapter::SettingsChange {
            model: Some(model.into()),
            effort: Some(effort.into()),
            ..Default::default()
        });
        assert_eq!(
            fake.adapter.validate_transmission(&t).is_ok(),
            ok,
            "{model}/{effort}"
        );
    }
    fake.close().await;
}

const PAGED_PROBE: &str = r#"#!/usr/bin/env python3
import json, sys, pathlib
d = pathlib.Path(__file__).parent
if '--version' in sys.argv:
    print('codex-cli 0.164.0'); sys.exit(0)
def model(i):
    return {"id": i, "model": i, "displayName": i, "description": "d", "hidden": False,
            "isDefault": i == "gpt-6.1-sol", "defaultReasoningEffort": "low",
            "supportedReasoningEfforts": [{"reasoningEffort": "low", "description": "l"}],
            "inputModalities": ["text"]}
for line in sys.stdin:
    msg = json.loads(line)
    (d / 'requests.log').open('a').write(line)
    m = msg.get('method'); i = msg.get('id'); p = msg.get('params') or {}
    if m == 'initialize':
        print(json.dumps({'id': i, 'result': {}}), flush=True)
    elif m == 'model/list':
        if p.get('cursor') == 'next-page':
            r = {'data': [model('gpt-6-luna')], 'nextCursor': None}
        else:
            r = {'data': [model('gpt-6.1-sol')], 'nextCursor': 'next-page'}
        print(json.dumps({'id': i, 'result': r}), flush=True)
"#;

#[tokio::test]
async fn discover_reads_every_model_list_page() {
    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("fake-codex");
    fake_exec::write_executable(&exe, PAGED_PROBE, 0o700);
    let catalog = tokio::time::timeout(
        WAIT,
        canterlot_worker::adapter::codex::discover(&CodexConfig {
            executable: exe,
            ..Default::default()
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let ids: Vec<String> = catalog.models().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, ["gpt-6.1-sol", "gpt-6-luna"]);
}

#[tokio::test]
async fn failed_history_lookup_never_sends_the_input_again() {
    let mut lines = handshake("thread-mini");
    lines.push(w_in(10, "thread/turns/list", json!({})));
    lines.push(w_out(
        json!({"id": 10, "error": {"code": -32000, "message": "busy"}}),
    ));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let mut t = fake.tx("retry");
    t.attempt = 2;
    let outcome = tokio::time::timeout(WAIT, fake.adapter.transmit(&t, &fake.group))
        .await
        .unwrap();
    assert!(
        matches!(outcome, TransmitOutcome::Undetermined),
        "{outcome:?}"
    );
    assert!(!fake.stdin_methods().contains(&"turn/start".to_owned()));
    // Хода нет и не будет: исполнение не остаётся занятым.
    fake.finish(t.command_id).await;
    assert!(matches!(
        fake.event().await,
        AdapterEvent::Phase {
            phase: canterlot_worker::adapter::Phase::AwaitingInput,
            ..
        }
    ));
    fake.close().await;
}

#[tokio::test]
async fn retried_input_found_in_a_finished_turn_finishes_at_once_and_queue_moves_on() {
    // Форма хода после возобновления: убитый ход — `interrupted` (ev/resume.jsonl:107);
    // `inProgress` от убитого процесса этот процесс тоже не ведёт.
    for status in ["interrupted", "inProgress", "completed"] {
        let command = Uuid::now_v7();
        let mut lines = handshake("thread-mini");
        lines.push(w_in(10, "thread/turns/list", json!({})));
        lines.push(w_out(json!({"id": 10, "result": {"data": [{
            "id": "turn-old", "status": status, "error": null,
            "items": [{"id": "u-old", "type": "userMessage", "clientId": command.to_string(),
                "content": []}],
        }], "nextCursor": null}})));
        lines.extend(turn_exchange(11, "thread-mini", "turn-next"));
        lines.push(w_out(json!({
            "method": "turn/completed",
            "params": {"threadId": "thread-mini",
                "turn": {"id": "turn-next", "status": "completed", "items": []}},
        })));
        let mut fake = Fake::new(lines);
        fake.start().await;
        let retry = Transmission {
            command_id: command,
            attempt: 2,
            text: "again".into(),
            files: vec![],
            settings: None,
        };
        match tokio::time::timeout(WAIT, fake.adapter.transmit(&retry, &fake.group))
            .await
            .unwrap()
        {
            TransmitOutcome::Accepted(proof) => {
                assert_eq!(proof.kind, "codex_history_client_id", "{status}")
            }
            other => panic!("{status}: {other:?}"),
        }
        let finished = tokio::time::timeout(Duration::from_secs(3), fake.finish(command))
            .await
            .unwrap_or_else(|_| panic!("{status}: no RunFinished for the retried input"));
        assert!(matches!(
            finished.last(),
            Some(AdapterEvent::RunFinished { usage, .. }) if usage.is_empty()
        ));
        assert!(matches!(
            fake.event().await,
            AdapterEvent::Phase {
                phase: canterlot_worker::adapter::Phase::AwaitingInput,
                ..
            }
        ));
        assert!(
            !fake.stdin_methods().contains(&"turn/start".to_owned()),
            "{status}: input not sent again"
        );
        // Очередь идёт дальше: следующий ввод не упирается в turn_active.
        let (_, events) = fake.send("next").await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AdapterEvent::RunFinished { .. })),
            "{status}"
        );
        fake.close().await;
    }
}

#[tokio::test]
async fn refused_terminal_cleanup_after_interrupt_is_reported() {
    use canterlot_worker::adapter::InterruptOutcome;
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(started(command_item("exec-1", "inProgress", None)));
    lines.push(w_in(11, "turn/interrupt", json!({})));
    lines.push(w_out(json!({"id": 11, "result": {}})));
    // Порядок записи: конец хода раньше списка терминалов (ev/interrupt_cleanup.jsonl:31-37).
    lines.push(note(
        "turn/completed",
        json!({"threadId": "thread-mini", "turn": {"id": "turn-1", "status": "interrupted", "items": []}}),
    ));
    lines.push(w_in(12, "thread/backgroundTerminals/list", json!({})));
    lines.push(w_out(
        json!({"id": 12, "result": {"data": [{"itemId": "exec-1", "processId": "p-1"}]}}),
    ));
    lines.push(w_in(13, "thread/backgroundTerminals/terminate", json!({})));
    lines.push(w_out(
        json!({"id": 13, "error": {"code": -32000, "message": "terminate refused"}}),
    ));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let id = fake.start_turn("run").await;
    match tokio::time::timeout(WAIT, fake.adapter.interrupt())
        .await
        .unwrap()
    {
        InterruptOutcome::Acknowledged => {}
        other => panic!("interrupt: {other:?}"),
    }
    let mut events = fake.finish(id).await;
    // Конец хода уже в очереди к моменту отказа: диагностика идёт за ним.
    events.push(fake.event().await);
    events.push(fake.event().await);
    assert!(
        events.iter().any(|event| matches!(
            event,
            AdapterEvent::Diagnostic { kind, .. } if *kind == "interrupt_cleanup_failed"
        )),
        "{events:?}"
    );
    assert!(
        fake.stdin_methods()
            .contains(&"thread/backgroundTerminals/terminate".to_owned())
    );
    fake.close().await;
}

#[tokio::test]
async fn interrupt_terminates_the_running_command_in_the_captured_order() {
    use canterlot_worker::adapter::InterruptOutcome;
    // Конец хода приходит раньше ответа списка, а `sleep 45` ещё жив.
    let mut fake = Fake::new(evidence_slice("interrupt_cleanup", 1, 37));
    fake.start().await;
    let id = fake.start_turn("run").await;
    match tokio::time::timeout(WAIT, fake.adapter.interrupt())
        .await
        .unwrap()
    {
        InterruptOutcome::Acknowledged => {}
        other => panic!("interrupt: {other:?}"),
    }
    fake.finish(id).await;
    let terminate = fake
        .stdin_log()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|line| line["method"] == "thread/backgroundTerminals/terminate")
        .expect("the interrupted command is terminated");
    assert_eq!(terminate["params"]["processId"], "39207");
    fake.close().await;
}

fn user_message(thread: &str, turn: &str) -> Vec<String> {
    let item = json!({"id": "u-late", "type": "userMessage", "clientId": "late", "content": []});
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

fn turn_response(request_id: i64, turn: &str) -> Vec<String> {
    vec![
        w_in(request_id, "turn/start", json!({})),
        w_out(json!({"id": request_id, "result": {"turn": {"id": turn, "status": "inProgress"}}})),
    ]
}

fn turn_end(thread: &str, turn: &str, status: &str) -> String {
    note(
        "turn/completed",
        json!({"threadId": thread, "turn": {"id": turn, "status": status, "items": []}}),
    )
}

async fn transmit(fake: &mut Fake, text: &str, limit: Duration) -> (Uuid, TransmitOutcome) {
    let t = fake.tx(text);
    let id = t.command_id;
    let outcome = tokio::time::timeout(limit, fake.adapter.transmit(&t, &fake.group))
        .await
        .expect("transmit stays within its limit");
    (id, outcome)
}

fn turn_started(thread: &str, turn: &str) -> String {
    note(
        "turn/started",
        json!({"threadId": thread, "turn": {"id": turn, "status": "inProgress", "items": []}}),
    )
}

#[tokio::test]
async fn turn_started_before_the_response_keeps_the_session_running() {
    let mut lines = handshake("thread-mini");
    lines.push(w_in(10, "turn/start", json!({})));
    lines.push(turn_started("thread-mini", "turn-1"));
    lines.push(w_out(
        json!({"id": 10, "result": {"turn": {"id": "turn-1", "status": "inProgress"}}}),
    ));
    lines.extend(user_message("thread-mini", "turn-1"));
    lines.push(turn_end("thread-mini", "turn-1", "completed"));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let id = fake.start_turn("reversed").await;
    let events = fake.finish(id).await;
    assert!(
        !events.iter().any(
            |event| matches!(event, AdapterEvent::Diagnostic { kind, .. } if *kind == "turn_merged")
        ),
        "{events:?}"
    );
    fake.close().await;
}

#[tokio::test]
async fn user_message_before_the_response_proves_delivery() {
    // На следующих ходах userMessage идёт через 10-20 мс после ответа, и его
    // пишет другая задача сервера: обратный порядок не должен терять доказательство.
    let mut lines = handshake("thread-mini");
    lines.push(w_in(10, "turn/start", json!({})));
    lines.push(turn_started("thread-mini", "turn-1"));
    lines.extend(user_message("thread-mini", "turn-1"));
    lines.push(w_out(
        json!({"id": 10, "result": {"turn": {"id": "turn-1", "status": "inProgress"}}}),
    ));
    lines.push(turn_end("thread-mini", "turn-1", "completed"));
    let mut fake = Fake::with_deadline(lines, Duration::from_secs(2));
    fake.start().await;
    let id = fake.start_turn("reversed").await;
    fake.finish(id).await;
    fake.close().await;
}

#[tokio::test]
async fn adopted_turn_ending_without_a_response_frees_the_next_input() {
    let mut lines = handshake("thread-mini");
    lines.push(w_in(10, "turn/start", json!({})));
    lines.push(turn_started("thread-mini", "turn-1"));
    lines.extend(user_message("thread-mini", "turn-1"));
    lines.push(turn_end("thread-mini", "turn-1", "completed"));
    lines.extend(turn_response(11, "turn-2"));
    lines.extend(user_message("thread-mini", "turn-2"));
    lines.push(turn_end("thread-mini", "turn-2", "completed"));
    let mut fake = Fake::with_deadline(lines, Duration::from_millis(300));
    fake.start().await;
    let id = fake.start_turn("first").await;
    fake.finish(id).await;
    let id = fake.start_turn("second").await;
    fake.finish(id).await;
    fake.close().await;
}

#[tokio::test]
async fn late_turn_start_response_adopts_the_turn() {
    let mut lines = handshake("thread-mini");
    lines.push(w_in(10, "turn/start", json!({})));
    lines.push(w("sleep", json!(600)));
    lines.push(w_out(
        json!({"id": 10, "result": {"turn": {"id": "turn-1", "status": "inProgress"}}}),
    ));
    lines.push(turn_started("thread-mini", "turn-1"));
    lines.extend(user_message("thread-mini", "turn-1"));
    lines.push(turn_end("thread-mini", "turn-1", "completed"));
    let mut fake = Fake::with_deadline(lines, Duration::from_millis(150));
    fake.start().await;
    let (id, outcome) = transmit(&mut fake, "slow", WAIT).await;
    assert!(
        matches!(outcome, TransmitOutcome::Undetermined),
        "{outcome:?}"
    );
    let events = fake.finish(id).await;
    assert!(
        events.iter().any(
            |event| matches!(event, AdapterEvent::InputAccepted { command_id } if *command_id == id)
        ),
        "{events:?}"
    );
    fake.close().await;
}

#[tokio::test]
async fn turn_started_after_the_deadline_adopts_the_turn_before_the_response() {
    let mut lines = handshake("thread-mini");
    lines.push(w_in(10, "turn/start", json!({})));
    lines.push(w("sleep", json!(600)));
    lines.push(turn_started("thread-mini", "turn-1"));
    lines.extend(user_message("thread-mini", "turn-1"));
    lines.push(w_out(
        json!({"id": 10, "result": {"turn": {"id": "turn-1", "status": "inProgress"}}}),
    ));
    lines.push(turn_end("thread-mini", "turn-1", "completed"));
    let mut fake = Fake::with_deadline(lines, Duration::from_millis(150));
    fake.start().await;
    let (id, outcome) = transmit(&mut fake, "slow", WAIT).await;
    assert!(
        matches!(outcome, TransmitOutcome::Undetermined),
        "{outcome:?}"
    );
    let events = fake.finish(id).await;
    assert!(
        events.iter().any(
            |event| matches!(event, AdapterEvent::InputAccepted { command_id } if *command_id == id)
        ),
        "{events:?}"
    );
    fake.close().await;
}

#[tokio::test]
async fn late_turn_start_refusal_frees_the_queue() {
    let mut lines = handshake("thread-mini");
    lines.push(w_in(10, "turn/start", json!({})));
    lines.push(w("sleep", json!(600)));
    lines.push(w_out(
        json!({"id": 10, "error": {"code": -32000, "message": "refused"}}),
    ));
    let mut fake = Fake::with_deadline(lines, Duration::from_millis(150));
    fake.start().await;
    let (id, outcome) = transmit(&mut fake, "slow", WAIT).await;
    assert!(
        matches!(outcome, TransmitOutcome::Undetermined),
        "{outcome:?}"
    );
    fake.finish(id).await;
    assert!(matches!(
        fake.event().await,
        AdapterEvent::Phase {
            phase: canterlot_worker::adapter::Phase::AwaitingInput,
            ..
        }
    ));
    fake.close().await;
}

#[tokio::test]
async fn server_turn_end_returns_the_phase_to_awaiting_input() {
    // Ход без ввода canterlot (продолжение после субагента): фаза Generating не должна остаться.
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(turn_end("thread-mini", "turn-1", "completed"));
    lines.push(turn_started("thread-mini", "turn-server"));
    lines.push(turn_end("thread-mini", "turn-server", "completed"));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let id = fake.start_turn("run").await;
    fake.finish(id).await;
    let mut last = None;
    for _ in 0..3 {
        last = Some(fake.event().await);
    }
    assert!(
        matches!(
            last,
            Some(AdapterEvent::Phase {
                phase: canterlot_worker::adapter::Phase::AwaitingInput,
                ..
            })
        ),
        "{last:?}"
    );
    fake.close().await;
}

#[tokio::test]
async fn late_user_message_confirms_an_unconfirmed_input() {
    let mut lines = handshake("thread-mini");
    lines.extend(turn_response(10, "turn-1"));
    lines.push(w("sleep", json!(600)));
    lines.extend(user_message("thread-mini", "turn-1"));
    lines.push(turn_end("thread-mini", "turn-1", "completed"));
    let mut fake = Fake::with_deadline(lines, Duration::from_millis(150));
    fake.start().await;
    let (id, outcome) = transmit(&mut fake, "late", WAIT).await;
    assert!(
        matches!(outcome, TransmitOutcome::Undetermined),
        "{outcome:?}"
    );
    let events = fake.finish(id).await;
    assert!(
        events.iter().any(
            |event| matches!(event, AdapterEvent::InputAccepted { command_id } if *command_id == id)
        ),
        "{events:?}"
    );
    fake.close().await;
}

#[tokio::test]
async fn turn_ending_without_a_user_message_leaves_the_proof_wait_at_once() {
    let mut lines = handshake("thread-mini");
    lines.extend(turn_response(10, "turn-1"));
    lines.push(turn_end("thread-mini", "turn-1", "failed"));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let (id, outcome) = transmit(&mut fake, "blocked", Duration::from_secs(5)).await;
    assert!(
        matches!(outcome, TransmitOutcome::Undetermined),
        "{outcome:?}"
    );
    fake.finish(id).await;
    fake.close().await;
}

fn awaits_owner(events: &[AdapterEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            AdapterEvent::RequestOpened { .. }
                | AdapterEvent::Phase {
                    phase: canterlot_worker::adapter::Phase::AwaitingPermission
                        | canterlot_worker::adapter::Phase::AwaitingAnswer,
                    ..
                }
        )
    })
}

fn waiting_status(thread: &str) -> String {
    note(
        "thread/status/changed",
        json!({"threadId": thread, "status": {"type": "active", "activeFlags": ["waitingOnApproval"]}}),
    )
}

fn child_approval(id: i64) -> String {
    w_out(
        json!({"id": id, "method": "item/commandExecution/requestApproval",
        "params": {"threadId": "thread-child", "turnId": "turn-child", "itemId": "exec-child",
            "command": "cargo test", "cwd": "/tmp"}}),
    )
}

#[tokio::test]
async fn request_outside_a_turn_is_refused_without_a_card_or_a_phase() {
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(turn_end("thread-mini", "turn-1", "completed"));
    lines.push(waiting_status("thread-mini"));
    lines.push(w_out(
        json!({"id": 30, "method": "mcpServer/elicitation/request",
        "params": {"serverName": "s", "threadId": "thread-mini", "turnId": null, "mode": "url",
            "message": "Open.", "url": "https://example.invalid/a", "elicitationId": "e-1"}}),
    ));
    lines.push(child_approval(31));
    lines.push(w_out(
        json!({"id": 32, "method": "item/tool/requestUserInput",
        "params": {"threadId": "thread-child", "turnId": "turn-child", "itemId": "ask-child",
            "questions": [{"id": "q", "question": "Which?", "header": "h"}]}}),
    ));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let (_, mut events) = fake.send("run").await;
    let (url, more) = fake.reply_to(30).await;
    events.extend(more);
    let (command, more) = fake.reply_to(31).await;
    events.extend(more);
    let (question, more) = fake.reply_to(32).await;
    events.extend(more);
    assert_eq!(
        url["result"],
        json!({"action": "decline", "content": null, "_meta": null})
    );
    assert_eq!(command["result"], json!({"decision": "decline"}));
    assert_eq!(question["result"], json!({"answers": {}}));
    assert!(!awaits_owner(&events), "{events:?}");
    fake.close().await;
}

#[tokio::test]
async fn open_subagent_request_is_refused_when_the_main_turn_ends() {
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(child_approval(30));
    lines.push(turn_end("thread-mini", "turn-1", "completed"));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let (_, events) = fake.send("run").await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::RequestOpened { .. })),
        "{events:?}"
    );
    let (reply, _) = fake.reply_to(30).await;
    assert_eq!(reply["result"], json!({"decision": "decline"}));
    fake.close().await;
}

#[tokio::test]
async fn interrupt_refuses_open_subagent_requests() {
    use canterlot_worker::adapter::InterruptOutcome;
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(child_approval(30));
    lines.push(w_in(11, "turn/interrupt", json!({})));
    lines.push(w_out(json!({"id": 11, "result": {}})));
    lines.push(turn_end("thread-mini", "turn-1", "interrupted"));
    let mut fake = Fake::new(lines);
    fake.start().await;
    fake.start_turn("run").await;
    loop {
        if matches!(fake.event().await, AdapterEvent::RequestOpened { .. }) {
            break;
        }
    }
    match tokio::time::timeout(WAIT, fake.adapter.interrupt())
        .await
        .unwrap()
    {
        InterruptOutcome::Acknowledged => {}
        other => panic!("interrupt: {other:?}"),
    }
    let (reply, _) = fake.reply_to(30).await;
    assert_eq!(reply["result"], json!({"decision": "decline"}));
    fake.close().await;
}

#[tokio::test]
async fn mcp_tool_approval_waits_for_the_owner_in_ask_mode() {
    let request = serde_json::from_str::<Value>(
        std::fs::read_to_string(repo_fixtures().join("elicitation.jsonl"))
            .unwrap()
            .lines()
            .nth(5)
            .unwrap(),
    )
    .unwrap()["data"]
        .clone();
    let mut request = request;
    request["params"]["threadId"] = json!("thread-mini");
    request["params"]["turnId"] = json!("turn-1");
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(w_out(request));
    lines.push(note(
        "serverRequest/resolved",
        json!({"threadId": "thread-mini", "requestId": 12}),
    ));
    lines.push(turn_end("thread-mini", "turn-1", "completed"));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let id = fake.start_turn("save the preset").await;
    let (request_id, payload) = loop {
        match fake.event().await {
            AdapterEvent::RequestOpened {
                request_id,
                kind,
                payload,
            } if kind == "permission" => break (request_id, payload),
            AdapterEvent::RunFinished { .. } => panic!("the owner saw no card"),
            _ => {}
        }
    };
    assert_eq!(payload["server"], "canterlot");
    assert!(matches!(
        fake.event().await,
        AdapterEvent::Phase {
            phase: canterlot_worker::adapter::Phase::AwaitingPermission,
            ..
        }
    ));
    fake.decide(&request_id, &allow()).await;
    // Сначала конец хода: ожидание ответа разбирает события и забрало бы его.
    fake.finish(id).await;
    let (reply, _) = fake.reply_to(12).await;
    assert_eq!(
        reply["result"],
        json!({"action": "accept", "content": {}, "_meta": null})
    );
    fake.close().await;
}

#[tokio::test]
async fn subagent_file_change_approval_carries_the_diff() {
    let change = json!({"id": "patch-child", "type": "fileChange", "status": "inProgress",
        "changes": [{"path": "/tmp/a.txt", "kind": {"type": "add"}, "diff": "hi\n"}]});
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(note(
        "item/started",
        json!({"threadId": "thread-child", "turnId": "turn-child", "item": change}),
    ));
    lines.push(w_out(
        json!({"id": 0, "method": "item/fileChange/requestApproval",
        "params": {"threadId": "thread-child", "turnId": "turn-child",
            "itemId": "patch-child", "reason": null, "grantRoot": null}}),
    ));
    let mut fake = Fake::new(lines);
    fake.start().await;
    fake.start_turn("spawn").await;
    let payload = loop {
        match fake.event().await {
            AdapterEvent::RequestOpened { payload, .. } => break payload,
            AdapterEvent::RunFinished { .. } => panic!("request never opened"),
            _ => {}
        }
    };
    assert_eq!(payload["agent"], "subagent");
    assert_eq!(payload["changes"][0]["diff"], "hi\n", "{payload}");
}

#[tokio::test]
async fn refused_request_is_declined_without_the_owner() {
    let change = json!({"id": "patch-child", "type": "fileChange", "status": "inProgress",
        "changes": [{"path": "/tmp/a.txt", "kind": {"type": "add"}, "diff": "hi\n"}]});
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(note(
        "item/started",
        json!({"threadId": "thread-child", "turnId": "turn-child", "item": change}),
    ));
    lines.push(w_out(
        json!({"id": 0, "method": "item/fileChange/requestApproval",
        "params": {"threadId": "thread-child", "turnId": "turn-child",
            "itemId": "patch-child", "reason": null, "grantRoot": null}}),
    ));
    let mut fake = Fake::new(lines);
    fake.start().await;
    fake.start_turn("spawn").await;
    let request_id = loop {
        match fake.event().await {
            AdapterEvent::RequestOpened { request_id, .. } => break request_id,
            AdapterEvent::RunFinished { .. } => panic!("request never opened"),
            _ => {}
        }
    };
    fake.adapter.refuse_request(&request_id, "too large").await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while !fake.stdin_log().contains(r#""decision":"decline""#) {
            let _ =
                tokio::time::timeout(Duration::from_millis(50), fake.adapter.next_event()).await;
        }
    })
    .await
    .expect("the server must get an explicit decline");
}

#[tokio::test]
async fn effort_only_change_is_checked_against_the_current_model() {
    let mut fake = Fake::new(handshake("thread-mini"));
    fake.start().await;
    let mut t = fake.tx("hi");
    // Модель сессии gpt-6-luna: у неё нет ultra, у модели по умолчанию есть.
    for (effort, ok) in [("ultra", false), ("max", true)] {
        t.settings = Some(adapter::SettingsChange {
            effort: Some(effort.into()),
            ..Default::default()
        });
        assert_eq!(
            fake.adapter.validate_transmission(&t).is_ok(),
            ok,
            "{effort}"
        );
    }
    fake.close().await;
}

#[tokio::test]
async fn rejected_turn_keeps_the_previous_settings() {
    let mut lines = handshake("thread-mini");
    lines.push(w_in(10, "turn/start", json!({})));
    lines.push(w_out(
        json!({"id": 10, "error": {"code": -32600, "message": "effort not allowed"}}),
    ));
    lines.extend(turn_exchange(11, "thread-mini", "turn-1"));
    lines.push(note(
        "turn/completed",
        json!({"threadId": "thread-mini", "turn": {"id": "turn-1", "status": "completed", "items": []}}),
    ));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let mut t = fake.tx("hi");
    t.settings = Some(adapter::SettingsChange {
        effort: Some("high".into()),
        permission_mode: Some("bypass".into()),
        ..Default::default()
    });
    let out = tokio::time::timeout(WAIT, fake.adapter.transmit(&t, &fake.group))
        .await
        .unwrap();
    assert!(matches!(out, TransmitOutcome::Rejected(_)), "{out:?}");
    fake.send("plain").await;
    let starts: Vec<Value> = fake
        .stdin_log()
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|message| message["method"] == "turn/start")
        .map(|message| message["params"].clone())
        .collect();
    assert_eq!(starts.len(), 2);
    assert_eq!(
        starts[1]["effort"], "low",
        "rejected effort not kept: {}",
        starts[1]
    );
    fake.close().await;
}

#[tokio::test]
async fn restart_resumes_thread_by_id() {
    let mut fake = Fake::new(evidence_slice("resume", 38, 73));
    fake.spec.native_conversation_id = Some("01a171c3-4681-7b81-bd8b-be81cac722c8".into());
    let native = fake.start().await;
    assert_eq!(native, "01a171c3-4681-7b81-bd8b-be81cac722c8");
    let methods = fake.stdin_methods();
    assert!(methods.contains(&"thread/resume".to_owned()));
    assert!(!methods.contains(&"thread/start".to_owned()));
    fake.close().await;
}

#[cfg(feature = "synthetic-adapter")]
#[test]
fn catalog_strings_are_owned_and_synthetic_catalog_unchanged() {
    let catalog = model_list_catalog();
    let entries = adapter::catalog_with(None, None, Some(&catalog), None);
    let codex = entries
        .iter()
        .find(|entry| entry.harness == "codex")
        .unwrap();
    let first: &String = &codex.models[0].id;
    assert_eq!(first, "gpt-6.1-sol");
    let entries = adapter::catalog();
    let synthetic = entries
        .iter()
        .find(|entry| entry.harness == "synthetic-codex")
        .unwrap();
    assert_eq!(synthetic.provider, "codex");
    assert!(
        synthetic
            .models
            .iter()
            .any(|model| model.id == "synthetic-large")
    );
}

fn error_text(events: &[AdapterEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            AdapterEvent::BlockStarted {
                kind: BlockKind::Error,
                ..
            } => Some("error-block"),
            AdapterEvent::BlockFinal { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("|")
}

#[tokio::test]
async fn start_refused_when_cli_missing_or_old_or_broken() {
    use canterlot_worker::adapter::AdapterError;
    let dir = TempDir::new().unwrap();
    for (executable, expected) in [
        (PathBuf::from("/nonexistent-codex-binary"), "cli_not_found"),
        (dir.path().join("old"), "cli_too_old"),
        (dir.path().join("broken"), "cli_version_failed"),
    ] {
        if expected == "cli_too_old" {
            fake_exec::write_executable(&executable, "#!/bin/sh\necho 'codex-cli 0.1.0'\n", 0o700);
        }
        if expected == "cli_version_failed" {
            fake_exec::write_executable(&executable, "#!/bin/sh\nexit 1\n", 0o700);
        }
        let context = AdapterContext {
            transmit_deadline: WAIT,
            exec_dir: dir.path().join("exec"),
            claude_code: None,
            codex: Some(CodexConfig {
                executable,
                ..Default::default()
            }),
            opencode: None,
        };
        let mut adapter = adapter::registry_with("codex", &context).expect("Codex registered");
        let mut spec = StartSpec {
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
        };
        spec.workdir = dir.path().into();
        let group = TreeContainment::new().unwrap();
        match tokio::time::timeout(WAIT, adapter.start(&spec, &group))
            .await
            .unwrap()
        {
            Err(AdapterError::StartFailed(reason)) => assert_eq!(reason, expected),
            other => panic!("expected {expected}, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn start_fails_when_app_server_exits_at_once() {
    use canterlot_worker::adapter::AdapterError;
    let dir = TempDir::new().unwrap();
    let exe = dir.path().join("quitter");
    fake_exec::write_executable(
        &exe,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'codex-cli 0.164.0'; exit 0; fi\nexit 1\n",
        0o700,
    );
    let context = AdapterContext {
        transmit_deadline: WAIT,
        exec_dir: dir.path().join("exec"),
        claude_code: None,
        codex: Some(CodexConfig {
            executable: exe,
            ..Default::default()
        }),
        opencode: None,
    };
    let mut adapter = adapter::registry_with("codex", &context).expect("Codex registered");
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
    };
    let group = TreeContainment::new().unwrap();
    match tokio::time::timeout(WAIT, adapter.start(&spec, &group))
        .await
        .unwrap()
    {
        Err(AdapterError::StartFailed(reason)) => assert_eq!(reason, "app_server_failed"),
        other => panic!("expected app_server_failed, got {other:?}"),
    }
}

#[tokio::test]
async fn start_refused_when_not_logged_in() {
    let mut lines = handshake("thread-mini");
    lines[4] = w_out(json!({"id": 1, "result": {"account": null, "requiresOpenaiAuth": true}}));
    let mut fake = Fake::new(lines);
    use canterlot_worker::adapter::AdapterError;
    match tokio::time::timeout(WAIT, fake.adapter.start(&fake.spec, &fake.group))
        .await
        .unwrap()
    {
        Err(AdapterError::StartFailed(reason)) => assert_eq!(reason, "auth_required"),
        other => panic!("expected auth_required, got {other:?}"),
    }
}

#[tokio::test]
async fn start_refused_when_resume_fails() {
    let mut lines = handshake("thread-mini");
    lines.pop();
    lines.push(w_in(3, "thread/resume", json!({})));
    lines.push(w_out(
        json!({"id": 3, "error": {"code": -32600, "message": "no such thread"}}),
    ));
    let mut fake = Fake::new(lines);
    fake.spec.native_conversation_id = Some("thread-mini".into());
    use canterlot_worker::adapter::AdapterError;
    match tokio::time::timeout(WAIT, fake.adapter.start(&fake.spec, &fake.group))
        .await
        .unwrap()
    {
        Err(AdapterError::StartFailed(reason)) => assert_eq!(reason, "resume_failed"),
        other => panic!("expected resume_failed, got {other:?}"),
    }
}

fn failed_turn(thread: &str, turn: &str, error: Value) -> Vec<String> {
    let mut lines = handshake(thread);
    lines.extend(turn_exchange(10, thread, turn));
    lines.push(w_out(json!({
        "method": "turn/completed",
        "params": {"threadId": thread, "turn": {"id": turn, "status": "failed", "error": error, "items": []}},
    })));
    lines
}

async fn run_failed_turn(lines: Vec<String>, text: &str) -> Vec<AdapterEvent> {
    let mut fake = Fake::new(lines);
    fake.start().await;
    let (_, events) = fake.send(text).await;
    fake.close().await;
    events
}

#[tokio::test]
async fn unauthorized_mid_turn_error_block() {
    let events = run_failed_turn(
        failed_turn(
            "thread-mini",
            "turn-1",
            json!({"codexErrorInfo": "unauthorized", "message": "login expired"}),
        ),
        "hi",
    )
    .await;
    assert!(error_text(&events).contains("codex login"), "{events:?}");
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "auth_required",
            required: false,
            ..
        }
    )));
}

#[tokio::test]
async fn unknown_model_maps_to_warning_error_and_finish() {
    let mut fake = Fake::new(evidence_slice("service_failure", 44, 71));
    fake.start().await;
    let (_, events) = fake.send("hi").await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockStarted {
                kind: BlockKind::Warning,
                ..
            }
        )),
        "model metadata warning block"
    );
    assert!(
        error_text(&events).contains("Модель не найдена"),
        "{events:?}"
    );
    fake.close().await;
}

#[tokio::test]
async fn retrying_errors_never_end_the_turn() {
    let mut fake = Fake::new(evidence_slice("service_failure", 1, 43));
    fake.start().await;
    let id = fake.start_turn("reply ok").await;
    let mut retries = 0;
    loop {
        let event = fake.event().await;
        match &event {
            AdapterEvent::Phase {
                phase: adapter::Phase::Generating,
                summary: Some(summary),
                ..
            } if summary.contains("повтор") => retries += 1,
            AdapterEvent::RunFinished {
                input_command_id, ..
            } if *input_command_id == id => break,
            AdapterEvent::RunFinished { .. } => {}
            _ => {}
        }
        if retries >= 6 {
            break;
        }
    }
    assert_eq!(retries, 6);
    use canterlot_worker::adapter::InterruptOutcome;
    match tokio::time::timeout(WAIT, fake.adapter.interrupt())
        .await
        .unwrap()
    {
        InterruptOutcome::Acknowledged => {}
        other => panic!("interrupt: {other:?}"),
    }
    let events = fake.finish(id).await;
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockStarted {
                kind: BlockKind::Error,
                ..
            }
        )),
        "retries alone are not an error"
    );
    fake.close().await;
}

#[tokio::test]
async fn server_error_classes_have_russian_names() {
    let events = run_failed_turn(
        failed_turn(
            "thread-mini",
            "turn-1",
            json!({"codexErrorInfo": {"serverOverloaded": {"httpStatusCode": 503}}, "message": "busy"}),
        ),
        "hi",
    )
    .await;
    assert!(
        error_text(&events).contains("Сервер перегружен (HTTP 503)"),
        "{events:?}"
    );
}

#[tokio::test]
async fn usage_limit_exceeded_reports_reached_and_error() {
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(w_out(json!({
        "method": "account/rateLimits/updated",
        "params": {
            "threadId": "thread-mini",
            "rateLimits": {
                "limitId": "codex",
                "primary": {"windowDurationMins": 300, "usedPercent": 50.0, "resetsAt": 4102444800u64},
                "secondary": {"windowDurationMins": 10080, "usedPercent": 100.0, "resetsAt": 4102444800u64},
            },
        },
    })));
    lines.push(w_out(json!({
        "method": "turn/completed",
        "params": {"threadId": "thread-mini", "turn": {"id": "turn-1", "status": "failed",
            "error": {"codexErrorInfo": "usageLimitExceeded", "message": "capped"}, "items": []}},
    })));
    let events = run_failed_turn(lines, "hi").await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Limit(canterlot_worker::adapter::LimitFact::Reached { .. })
    )));
    assert!(
        error_text(&events).contains("Исчерпан лимит Codex"),
        "{events:?}"
    );
}

#[tokio::test]
async fn rate_limit_exceeded_reads_fresh_limits() {
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(w_out(json!({
        "method": "turn/completed",
        "params": {"threadId": "thread-mini", "turn": {"id": "turn-1", "status": "failed",
            "error": {"codexErrorInfo": "rateLimitExceeded", "message": "slow down"}, "items": []}},
    })));
    let events = run_failed_turn(lines, "hi").await;
    // Свежий probe account/rateLimits/read дал окна: есть Observed и Reached.
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Limit(canterlot_worker::adapter::LimitFact::Reached { .. })
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Limit(canterlot_worker::adapter::LimitFact::Observed { .. })
    )));
}

// Ответ свежего чтения для зонда: тот же двойник проигрывает его по методу.
fn fresh_read_lines(result: Value) -> Vec<String> {
    vec![
        w_in(1, "account/rateLimits/read", json!({})),
        w_out(json!({"id": 1, "result": result})),
    ]
}

fn rate_limited_turn(read: Value) -> Vec<String> {
    let mut lines = failed_turn(
        "thread-mini",
        "turn-1",
        json!({"codexErrorInfo": "rateLimitExceeded", "message": "slow down"}),
    );
    lines.extend(fresh_read_lines(read));
    lines
}

fn quiet_read() -> Value {
    json!({"rateLimits": {"limitId": "codex",
        "primary": {"windowDurationMins": 300, "usedPercent": 40.0, "resetsAt": 1792927784},
        "secondary": {"windowDurationMins": 10080, "usedPercent": 10.0, "resetsAt": 1793514584}},
        "ordinaryUsageAllowed": true})
}

#[tokio::test]
async fn rate_limit_without_exhausted_window_is_an_error_block_without_reached() {
    let events = run_failed_turn(rate_limited_turn(quiet_read()), "hi").await;
    assert!(
        error_text(&events).contains("Codex ограничил частоту запросов"),
        "{events:?}"
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        AdapterEvent::Limit(canterlot_worker::adapter::LimitFact::Reached { .. })
    )));
}

#[tokio::test]
async fn usage_limit_with_a_failed_read_still_stops_the_queue() {
    let mut lines = failed_turn(
        "thread-mini",
        "turn-1",
        json!({"codexErrorInfo": "usageLimitExceeded", "message": "capped"}),
    );
    lines.push(w_in(1, "account/rateLimits/read", json!({})));
    lines.push(w_out(
        json!({"id": 1, "error": {"code": -32000, "message": "no route"}}),
    ));
    let events = run_failed_turn(lines, "hi").await;
    assert!(
        events.iter().any(|event| matches!(
            event,
            AdapterEvent::Limit(canterlot_worker::adapter::LimitFact::Reached { .. })
        )),
        "{events:?}"
    );
    assert!(error_text(&events).contains("Исчерпан лимит Codex"));
}

#[tokio::test]
async fn limit_failure_turn_end_survives_dropped_event_polls() {
    let mut fake = Fake::new(rate_limited_turn(quiet_read()));
    fake.start().await;
    let id = fake.start_turn("hi").await;
    // Как супервизор: next_event опрашивается коротко и бросается.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut events = vec![];
    while std::time::Instant::now() < deadline {
        if let Ok(event) =
            tokio::time::timeout(Duration::from_millis(1), fake.adapter.next_event()).await
        {
            let done = matches!(event, AdapterEvent::RunFinished { input_command_id, .. } if input_command_id == id);
            events.push(event);
            if done {
                break;
            }
        }
    }
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::RunFinished { .. })),
        "turn end lost: {events:?}"
    );
    assert!(matches!(
        events.last(),
        Some(AdapterEvent::RunFinished { .. })
    ));
    let after = fake.event().await;
    assert!(
        matches!(
            after,
            AdapterEvent::Phase {
                phase: canterlot_worker::adapter::Phase::AwaitingInput,
                ..
            }
        ),
        "{after:?}"
    );
    assert!(error_text(&events).contains("Codex ограничил частоту запросов"));
    fake.close().await;
}

#[tokio::test]
async fn context_window_exceeded_is_context_exhausted() {
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(w_out(json!({
        "method": "turn/completed",
        "params": {"threadId": "thread-mini", "turn": {"id": "turn-1", "status": "failed",
            "error": {"codexErrorInfo": "contextWindowExceeded", "message": "full"}, "items": []}},
    })));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let (_, events) = fake.send("hi").await;
    // ContextExhausted идёт сразу за RunFinished: учёт уже записан.
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AdapterEvent::RunFinished { .. }))
    );
    let mut saw_exhausted = false;
    for _ in 0..3 {
        let event = fake.event().await;
        if matches!(event, AdapterEvent::ContextExhausted) {
            saw_exhausted = true;
            break;
        }
    }
    assert!(saw_exhausted);
    fake.close().await;
}

#[tokio::test]
async fn interrupted_turn_finishes_without_error() {
    let mut fake = Fake::new(evidence_slice("interrupt", 1, 36));
    fake.start().await;
    let id = fake.start_turn("sleep please").await;
    use canterlot_worker::adapter::InterruptOutcome;
    match tokio::time::timeout(WAIT, fake.adapter.interrupt())
        .await
        .unwrap()
    {
        InterruptOutcome::Acknowledged => {}
        other => panic!("interrupt: {other:?}"),
    }
    let events = fake.finish(id).await;
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AdapterEvent::BlockStarted {
                kind: BlockKind::Error,
                ..
            }
        )),
        "{events:?}"
    );
    fake.close().await;
}

#[tokio::test]
async fn system_error_is_optional() {
    let mut fake = Fake::new(evidence_slice("service_failure", 44, 71));
    fake.start().await;
    let _ = fake.start_turn("hi").await;
    let mut saw_system_error = false;
    for _ in 0..30 {
        let event = fake.event().await;
        if matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "codex_system_error",
                required: false,
                ..
            }
        ) {
            saw_system_error = true;
            break;
        }
        if matches!(event, AdapterEvent::RunFinished { .. }) {
            break;
        }
    }
    assert!(saw_system_error);
    fake.close().await;
}

#[tokio::test]
async fn thread_closed_is_required() {
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(w_out(json!({
        "method": "thread/closed",
        "params": {"threadId": "thread-mini"},
    })));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let _ = fake.start_turn("hi").await;
    let mut saw = false;
    for _ in 0..10 {
        let event = fake.event().await;
        if matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "thread_closed",
                required: true,
                ..
            }
        ) {
            saw = true;
            break;
        }
        if matches!(event, AdapterEvent::RunFinished { .. }) {
            break;
        }
    }
    assert!(saw, "required thread_closed diagnostic");
    fake.close().await;
}

#[tokio::test]
async fn merged_turn_response_is_required_and_undetermined() {
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(w_out(json!({
        "method": "turn/completed",
        "params": {"threadId": "thread-mini",
            "turn": {"id": "turn-1", "status": "completed", "error": null, "items": []}},
    })));
    lines.push(w_in(11, "turn/start", json!({})));
    lines.push(w_out(
        json!({"id": 11, "result": {"turn": {"id": "turn-1", "status": "inProgress"}}}),
    ));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let t = Transmission {
        command_id: Uuid::now_v7(),
        attempt: 1,
        text: "hi".into(),
        files: vec![],
        settings: None,
    };
    match tokio::time::timeout(WAIT, fake.adapter.transmit(&t, &fake.group))
        .await
        .unwrap()
    {
        TransmitOutcome::Accepted(_) => {}
        other => panic!("first delivery: {other:?}"),
    }
    fake.finish(t.command_id).await;
    // Второй ответ переиспользует знакомый turn.id: очередь останавливается.
    let t2 = Transmission {
        command_id: Uuid::now_v7(),
        attempt: 1,
        text: "again".into(),
        files: vec![],
        settings: None,
    };
    match tokio::time::timeout(WAIT, fake.adapter.transmit(&t2, &fake.group))
        .await
        .unwrap()
    {
        TransmitOutcome::Undetermined => {}
        other => panic!("expected undetermined merge, got {other:?}"),
    }
    let mut saw_merge = false;
    for _ in 0..10 {
        let event = fake.event().await;
        if matches!(
            event,
            AdapterEvent::Diagnostic {
                kind: "turn_merged",
                required: true,
                ..
            }
        ) {
            saw_merge = true;
            break;
        }
    }
    assert!(saw_merge, "required turn_merged diagnostic");
    fake.close().await;
}

#[tokio::test]
async fn process_exit_mid_turn_is_undetermined_then_exited() {
    let mut lines = handshake("thread-mini");
    lines.push(w_in(10, "turn/start", json!({})));
    lines.push(w_out(
        json!({"id": 10, "result": {"turn": {"id": "turn-1", "status": "inProgress"}}}),
    ));
    lines.push(w("out", Value::String("@exit".into())));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let t = Transmission {
        command_id: Uuid::now_v7(),
        attempt: 1,
        text: "hi".into(),
        files: vec![],
        settings: None,
    };
    // Ответ на turn/start прочитан, дальше процесс умер: исход не определён.
    match tokio::time::timeout(WAIT, fake.adapter.transmit(&t, &fake.group))
        .await
        .unwrap()
    {
        TransmitOutcome::Undetermined => {}
        other => panic!("expected undetermined, got {other:?}"),
    }
    loop {
        let event = fake.event().await;
        if matches!(event, AdapterEvent::ProcessExited { .. }) {
            break;
        }
    }
}

#[tokio::test]
async fn unsupported_request_gets_error_reply_and_diagnostic() {
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(w_out(json!({
        "id": 20, "method": "attestation/generate", "params": {"threadId": "thread-mini"},
    })));
    lines.push(w_out(json!({
        "method": "turn/completed",
        "params": {"threadId": "thread-mini", "turn": {"id": "turn-1", "status": "completed", "error": null, "items": []}},
    })));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let (_, events) = fake.send("hi").await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "unsupported_server_request",
            required: false,
            ..
        }
    )));
    // Ответ уходит в трубу раньше конца хода, но двойник читает его сам по себе.
    let (reply, _) = fake.reply_to(20).await;
    assert_eq!(reply["error"]["code"], -32601);
    fake.close().await;
}

#[tokio::test]
async fn malformed_line_is_optional_and_stream_continues() {
    let mut lines = handshake("thread-mini");
    lines.extend(turn_exchange(10, "thread-mini", "turn-1"));
    lines.push(w("out", json!({"__raw__": "not json"})));
    lines.push(w_out(json!({
        "method": "turn/completed",
        "params": {"threadId": "thread-mini", "turn": {"id": "turn-1", "status": "completed", "error": null, "items": []}},
    })));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let (_, events) = fake.send("hi").await;
    assert!(events.iter().any(|event| matches!(
        event,
        AdapterEvent::Diagnostic {
            kind: "unparsed_line",
            required: false,
            ..
        }
    )));
    fake.close().await;
}

#[tokio::test]
async fn hung_server_that_stops_reading_cannot_block_the_supervisor() {
    let mut lines = handshake("thread-mini");
    // Сервер перестаёт читать stdin, а ввод больше буфера трубы.
    lines.push(w("sleep", json!(30_000)));
    let mut fake = Fake::with_deadline(lines, Duration::from_millis(300));
    fake.start().await;
    let mut t = fake.tx("x");
    t.text = "x".repeat(200_000);
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        fake.adapter.transmit(&t, &fake.group),
    )
    .await
    .expect("a hung pipe stops the supervisor loop");
    assert!(
        matches!(outcome, TransmitOutcome::Undetermined),
        "{outcome:?}"
    );
    loop {
        if matches!(fake.event().await, AdapterEvent::ProcessExited { .. }) {
            break;
        }
    }
    fake.close().await;
}

#[tokio::test]
async fn broken_pipe_is_undetermined() {
    let mut lines = handshake("thread-mini");
    lines.push(w_in(10, "turn/start", json!({})));
    lines.push(w_out(
        json!({"id": 10, "result": {"turn": {"id": "turn-1", "status": "inProgress"}}}),
    ));
    lines.push(w("out", Value::String("@exit".into())));
    let mut fake = Fake::new(lines);
    fake.start().await;
    let t = Transmission {
        command_id: Uuid::now_v7(),
        attempt: 1,
        text: "hi".into(),
        files: vec![],
        settings: None,
    };
    // Ответ прочитан, дальше процесс умер в ожидании доказательства.
    match tokio::time::timeout(WAIT, fake.adapter.transmit(&t, &fake.group))
        .await
        .unwrap()
    {
        TransmitOutcome::Undetermined => {}
        other => panic!("expected undetermined, got {other:?}"),
    }
    loop {
        let event = fake.event().await;
        if matches!(event, AdapterEvent::ProcessExited { .. }) {
            break;
        }
    }
    // После выхода: тоже не определено, а не отказ.
    let t = Transmission {
        command_id: Uuid::now_v7(),
        attempt: 1,
        text: "hi".into(),
        files: vec![],
        settings: None,
    };
    match tokio::time::timeout(WAIT, fake.adapter.transmit(&t, &fake.group))
        .await
        .unwrap()
    {
        TransmitOutcome::Undetermined => {}
        other => panic!("expected undetermined, got {other:?}"),
    }
}

fn smoke_flag(value: Option<&str>) -> bool {
    value == Some("1")
}

fn live_smoke_enabled() -> bool {
    smoke_flag(std::env::var("CANTERLOT_CODEX_SMOKE").ok().as_deref())
}

#[test]
fn live_smoke_is_skipped_without_the_flag() {
    assert!(!smoke_flag(None));
    assert!(!smoke_flag(Some("")));
    assert!(!smoke_flag(Some("true")));
    assert!(smoke_flag(Some("1")));
}

const PIXEL_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0,
    0, 0, 31, 21, 196, 137, 0, 0, 0, 10, 73, 68, 65, 84, 120, 156, 99, 0, 1, 0, 0, 5, 0, 1, 13, 10,
    45, 180, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

// Живой прогон владельца (НЕ запускается в обычных тестах): настоящий codex,
// временный git-репозиторий, профиль owner. Печатает подтверждённые факты
// рядом с пунктами OPEN плана; цена — около восьми дешёвых ходов.
#[tokio::test]
async fn live_smoke() {
    if !live_smoke_enabled() {
        eprintln!("SKIP: set CANTERLOT_CODEX_SMOKE=1 to run live Codex turns");
        return;
    }
    let model = std::env::var("CANTERLOT_CODEX_SMOKE_MODEL").unwrap_or("gpt-6-luna".into());
    let dir = TempDir::new().unwrap();
    let workdir = dir.path().join("work");
    std::fs::create_dir_all(&workdir).unwrap();
    std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&workdir)
        .status()
        .unwrap();
    let config = CodexConfig::default();
    let catalog = canterlot_worker::adapter::codex::discover(&config)
        .await
        .expect("discover against the real CLI");
    eprintln!(
        "SMOKE: models={} (catalog OPEN closed)",
        catalog.models.len()
    );
    assert!(catalog.models.iter().any(|entry| entry.id == model));
    let context = AdapterContext {
        transmit_deadline: Duration::from_secs(60),
        exec_dir: dir.path().join("exec"),
        claude_code: None,
        codex: Some(config),
        opencode: None,
    };
    let mut adapter = adapter::registry_with("codex", &context).expect("Codex registered");
    let group = TreeContainment::new().unwrap();
    let spec = StartSpec {
        execution_id: Uuid::now_v7(),
        generation: 1,
        workdir: workdir.clone(),
        native_conversation_id: None,
        profile: "codex".into(),
        model: model.clone(),
        effort: "low".into(),
        permission_mode: "ask".into(),
        instructions: canterlot_worker::adapter::InstructionSpec {
            mode: canterlot_worker::adapter::InstructionMode::Owner,
            text: String::new(),
            preset_id: None,
            revision: 0,
            title: String::new(),
        },
    };
    let started = tokio::time::timeout(Duration::from_secs(90), adapter.start(&spec, &group))
        .await
        .expect("start in time")
        .expect("live start works");
    eprintln!(
        "SMOKE: thread={} (start/resume OPEN checked below)",
        started.native_conversation_id
    );

    async fn send(
        adapter: &mut Box<dyn Adapter>,
        group: &TreeContainment,
        transmission: Transmission,
    ) -> (Uuid, Vec<AdapterEvent>) {
        let id = transmission.command_id;
        match tokio::time::timeout(
            Duration::from_secs(120),
            adapter.transmit(&transmission, group),
        )
        .await
        .expect("transmit in time")
        {
            TransmitOutcome::Accepted(proof) => assert_eq!(proof.command_id, id),
            other => panic!("live delivery: {other:?}"),
        }
        let mut events = vec![];
        loop {
            let event = tokio::time::timeout(Duration::from_secs(240), adapter.next_event())
                .await
                .expect("event in time");
            let done = matches!(
                event,
                AdapterEvent::RunFinished {
                    input_command_id,
                    ..
                } if input_command_id == id
            );
            events.push(event);
            if done {
                return (id, events);
            }
        }
    }
    fn text(transmission_text: &str) -> Transmission {
        Transmission {
            command_id: Uuid::now_v7(),
            attempt: 1,
            text: transmission_text.into(),
            files: vec![],
            settings: None,
        }
    }
    async fn open_request(adapter: &mut Box<dyn Adapter>, id: Uuid, kind: &str) -> (String, Value) {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(180), adapter.next_event())
                .await
                .expect("event in time");
            match &event {
                AdapterEvent::RequestOpened {
                    request_id,
                    kind: actual,
                    payload,
                } if actual == kind => return (request_id.clone(), payload.clone()),
                AdapterEvent::RunFinished {
                    input_command_id, ..
                } if *input_command_id == id => panic!("{kind} never opened"),
                _ => {}
            }
        }
    }

    // Текст без инструментов.
    let (_, events) = send(
        &mut adapter,
        &group,
        text("Reply with exactly CANTERLOT-SMOKE. Do not use tools."),
    )
    .await;
    assert!(Fake::final_text(&events).contains("CANTERLOT-SMOKE"));
    eprintln!("SMOKE: text turn ok");
    // Вопрос: отвечаем первым id из груза.
    let transmission = text(
        "Ask me one question with options Red and Blue using request_user_input, header Color.",
    );
    let qid = transmission.command_id;
    match tokio::time::timeout(
        Duration::from_secs(120),
        adapter.transmit(&transmission, &group),
    )
    .await
    .expect("transmit in time")
    {
        TransmitOutcome::Accepted(_) => {}
        other => panic!("live delivery: {other:?}"),
    }
    let (request_id, payload) = open_request(&mut adapter, qid, "question").await;
    let codex_id = payload["questions"][0]["id"].as_str().unwrap().to_owned();
    let mut answers = std::collections::BTreeMap::new();
    answers.insert(
        codex_id,
        canterlot_proto::requests::AnswerValue::Single("Blue".to_owned()),
    );
    match tokio::time::timeout(
        Duration::from_secs(60),
        adapter.deliver_decision(&request_id, &DecisionPayload::Question { answers }),
    )
    .await
    .expect("decision in time")
    {
        canterlot_worker::adapter::DecisionOutcome::Applied(_) => {}
        other => panic!("live decision: {other:?}"),
    }
    eprintln!("SMOKE: question answered (structured questions OPEN closed)");
    // Разрешение: перевод в read_only, команда создаёт файл, разрешаем.
    let mut approval =
        text("Create the file probe-smoke.txt with the single word SMOKE using a shell command.");
    approval.settings = Some(canterlot_worker::adapter::SettingsChange {
        model: None,
        effort: None,
        permission_mode: Some("read_only".into()),
        instructions: None,
    });
    let aid = approval.command_id;
    match tokio::time::timeout(
        Duration::from_secs(120),
        adapter.transmit(&approval, &group),
    )
    .await
    .expect("transmit in time")
    {
        TransmitOutcome::Accepted(_) => {}
        other => panic!("live delivery: {other:?}"),
    }
    let (request_id, _) = open_request(&mut adapter, aid, "permission").await;
    match tokio::time::timeout(
        Duration::from_secs(60),
        adapter.deliver_decision(&request_id, &allow()),
    )
    .await
    .expect("decision in time")
    {
        canterlot_worker::adapter::DecisionOutcome::Applied(_) => {}
        other => panic!("live decision: {other:?}"),
    }
    eprintln!("SMOKE: command approval allowed (approvals OPEN closed)");
    // Картинка-вложение.
    let png = workdir.join("red.png");
    std::fs::write(&png, PIXEL_PNG).unwrap();
    let mut image = text("What is the dominant color of the attached image? Reply with one word.");
    image.files = vec![canterlot_worker::adapter::LocalFile {
        path: png.clone(),
        name: "red.png".into(),
        mime: "image/png".into(),
        size: PIXEL_PNG.len() as u64,
        image: true,
    }];
    let (_, events) = send(&mut adapter, &group, image).await;
    assert!(!Fake::final_text(&events).is_empty());
    eprintln!("SMOKE: localImage turn ok (attachments OPEN closed)");
    // Прерывание долгого сна.
    let sleeping = text("Run the shell command `sleep 30` and then reply DONE.");
    let sid = sleeping.command_id;
    match tokio::time::timeout(
        Duration::from_secs(120),
        adapter.transmit(&sleeping, &group),
    )
    .await
    .expect("transmit in time")
    {
        TransmitOutcome::Accepted(_) => {}
        other => panic!("live delivery: {other:?}"),
    }
    tokio::time::sleep(Duration::from_secs(5)).await;
    match tokio::time::timeout(Duration::from_secs(30), adapter.interrupt())
        .await
        .expect("interrupt in time")
    {
        canterlot_worker::adapter::InterruptOutcome::Acknowledged => {}
        other => panic!("live interrupt: {other:?}"),
    }
    let _ = sid;
    eprintln!("SMOKE: interrupt acknowledged (interrupt OPEN closed)");
    // Кодовое слово и возобновление по id.
    let (_, events) = send(
        &mut adapter,
        &group,
        text("Remember this code word: ZEBRA-SMOKE. Reply OK."),
    )
    .await;
    assert!(Fake::final_text(&events).contains("OK"));
    tokio::time::timeout(
        Duration::from_secs(10),
        adapter.close(CloseMode::Graceful, &group),
    )
    .await
    .expect("close in time")
    .expect("live close works");
    let context = AdapterContext {
        transmit_deadline: Duration::from_secs(60),
        exec_dir: dir.path().join("exec2"),
        claude_code: None,
        codex: Some(CodexConfig::default()),
        opencode: None,
    };
    let mut adapter = adapter::registry_with("codex", &context).expect("Codex registered");
    let spec = StartSpec {
        execution_id: Uuid::now_v7(),
        generation: 2,
        workdir: workdir.clone(),
        native_conversation_id: Some(started.native_conversation_id.clone()),
        profile: "codex".into(),
        model: model.clone(),
        effort: "low".into(),
        permission_mode: "ask".into(),
        instructions: canterlot_worker::adapter::InstructionSpec {
            mode: canterlot_worker::adapter::InstructionMode::Owner,
            text: String::new(),
            preset_id: None,
            revision: 0,
            title: String::new(),
        },
    };
    let resumed = tokio::time::timeout(Duration::from_secs(90), adapter.start(&spec, &group))
        .await
        .expect("start in time")
        .expect("live resume works");
    assert_eq!(
        resumed.native_conversation_id,
        started.native_conversation_id
    );
    let (_, events) = send(
        &mut adapter,
        &group,
        text("What is the code word I asked you to remember? Reply with it only."),
    )
    .await;
    assert!(Fake::final_text(&events).contains("ZEBRA-SMOKE"));
    eprintln!("SMOKE: resume by id recalls the code word (resume OPEN closed)");
    // План 2c, задача 7, live: собственный CODEX_HOME под $HOME (предки владельца
    // существуют), настоящее окружение владельца, настоящий codex.
    let home_base = std::env::var("HOME").expect("smoke needs HOME");
    let smoke_home = PathBuf::from(format!(
        "{home_base}/canterlot-smoke-codex-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&smoke_home);
    let repo = smoke_home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap()
            .success()
    );
    std::fs::write(repo.join("AGENTS.md"), "# canterlot smoke repo\n").unwrap();
    let codex_home = smoke_home.join("codex-home");
    let home_config = || CodexConfig {
        executable: std::env::var_os("CANTERLOT_CODEX_EXECUTABLE")
            .map(PathBuf::from)
            .unwrap_or_else(|| "codex".into()),
        home_dir: Some(codex_home.clone()),
        owner_home: None,
        home_policy: TempDirPolicy::Refuse,
        ..Default::default()
    };
    let clean_instructions = || canterlot_worker::adapter::InstructionSpec {
        mode: canterlot_worker::adapter::InstructionMode::Clean,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    };
    let mut preset_adapter: Box<dyn Adapter> = adapter::registry_with(
        "codex",
        &AdapterContext {
            transmit_deadline: Duration::from_secs(60),
            exec_dir: smoke_home.join("exec"),
            claude_code: None,
            codex: Some(home_config()),
            opencode: None,
        },
    )
    .expect("Codex registered");
    let preset_group = TreeContainment::new().unwrap();
    let preset_spec = StartSpec {
        execution_id: Uuid::now_v7(),
        generation: 1,
        workdir: repo.clone(),
        native_conversation_id: None,
        profile: "codex".into(),
        model: model.clone(),
        effort: "low".into(),
        permission_mode: "ask".into(),
        instructions: clean_instructions(),
    };
    tokio::time::timeout(
        Duration::from_secs(90),
        preset_adapter.start(&preset_spec, &preset_group),
    )
    .await
    .expect("preset start in time")
    .expect("clean start under canterlot home");
    // Без канонария в «Без инструкций» — пусто.
    let (_, events) = send(
        &mut preset_adapter,
        &preset_group,
        text("Reply with exactly NONE if you have no canary word, else the word. No tools."),
    )
    .await;
    assert!(Fake::final_text(&events).contains("NONE"));
    eprintln!("SMOKE: clean preset starts under canterlot CODEX_HOME, no canary");
    // Второй app-server на том же доме отвечает account/read (OPEN 8).
    let mut second: Box<dyn Adapter> = adapter::registry_with(
        "codex",
        &AdapterContext {
            transmit_deadline: Duration::from_secs(60),
            exec_dir: smoke_home.join("exec-two"),
            claude_code: None,
            codex: Some(home_config()),
            opencode: None,
        },
    )
    .expect("Codex registered");
    let second_group = TreeContainment::new().unwrap();
    tokio::time::timeout(
        Duration::from_secs(90),
        second.start(&preset_spec, &second_group),
    )
    .await
    .expect("second start in time")
    .expect("second app-server answers account/read on the same home");
    tokio::time::timeout(
        Duration::from_secs(10),
        second.close(CloseMode::Graceful, &second_group),
    )
    .await
    .expect("close in time")
    .expect("second close works");
    eprintln!("SMOKE: two concurrent app-servers on one home both answer (OPEN 8 checked)");
    // Пресет с CANARY-DEV-A: перезапуск, resume, вопрос.
    let mut canary = text(
        "Reply with exactly the canary word from your developer instructions or NONE. No tools.",
    );
    canary.settings = Some(canterlot_worker::adapter::SettingsChange {
        model: None,
        effort: None,
        permission_mode: None,
        instructions: Some(canterlot_worker::adapter::InstructionSpec {
            mode: canterlot_worker::adapter::InstructionMode::Preset,
            text: "The canary word is CANARY-DEV-A.".into(),
            preset_id: Some(Uuid::now_v7()),
            revision: 1,
            title: "canary".into(),
        }),
    });
    let (_, events) = send(&mut preset_adapter, &preset_group, canary).await;
    assert!(Fake::final_text(&events).contains("CANARY-DEV-A"));
    eprintln!("SMOKE: preset text reaches the model, app-server restarts with resume");
    // «Как у владельца»: присутствует ли ещё A — вопрос OPEN 5, 6.
    let mut back_to_owner = text(
        "Reply with exactly the canary word from your developer instructions or NONE. No tools.",
    );
    back_to_owner.settings = Some(canterlot_worker::adapter::SettingsChange {
        model: None,
        effort: None,
        permission_mode: None,
        instructions: Some(canterlot_worker::adapter::InstructionSpec {
            mode: canterlot_worker::adapter::InstructionMode::Owner,
            text: String::new(),
            preset_id: None,
            revision: 0,
            title: String::new(),
        }),
    });
    let (_, events) = send(&mut preset_adapter, &preset_group, back_to_owner).await;
    eprintln!(
        "SMOKE: owner preset after canary answers: {} (OPEN 5, 6)",
        Fake::final_text(&events).trim()
    );
    // Назад в «Без инструкций»: снова NONE.
    let mut clean_again = text(
        "Reply with exactly the canary word from your developer instructions or NONE. No tools.",
    );
    clean_again.settings = Some(canterlot_worker::adapter::SettingsChange {
        model: None,
        effort: None,
        permission_mode: None,
        instructions: Some(clean_instructions()),
    });
    let (_, events) = send(&mut preset_adapter, &preset_group, clean_again).await;
    assert!(Fake::final_text(&events).contains("NONE"));
    tokio::time::timeout(
        Duration::from_secs(10),
        preset_adapter.close(CloseMode::Graceful, &preset_group),
    )
    .await
    .expect("close in time")
    .expect("preset close works");
    let _ = std::fs::remove_dir_all(&smoke_home);
    eprintln!("SMOKE: presets smoke done (context is covered by behavior, OPEN 8 checked)");
    tokio::time::timeout(
        Duration::from_secs(10),
        adapter.close(CloseMode::Graceful, &group),
    )
    .await
    .expect("close in time")
    .expect("live close works");
    eprintln!("SMOKE: done");
}
