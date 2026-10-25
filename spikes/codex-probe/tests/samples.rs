//! Разбор реальных строк протокола codex app-server 0.164.0, записанных пробой.

use codex_probe::framing::LineFramer;
use codex_probe::protocol::{
    CommandApprovalParams, ErrorNotification, FileChangeApprovalParams, Incoming, ItemNotification,
    RequestId, ServerRequestResolved, ThreadItem, TokenUsageUpdated, TurnNotification,
    TurnStartResult, TurnStatus, UserInputParams, classify,
};
use serde_json::Value;

const SAMPLES: &str = include_str!("fixtures/app_server_0.164.0.jsonl");

fn messages() -> Vec<Incoming> {
    let mut framer = LineFramer::new(1 << 20);
    // подаём фикстуру кусками по 7 байт, чтобы задеть разрывы внутри UTF-8 («’» в текстах)
    let mut out = Vec::new();
    for chunk in SAMPLES.as_bytes().chunks(7) {
        for line in framer.push(chunk) {
            let line = line.expect("valid line");
            out.push(classify(
                serde_json::from_str::<Value>(&line).expect("json"),
            ));
        }
    }
    assert!(framer.finish().is_none());
    out
}

fn notifications<'a>(all: &'a [Incoming], name: &str) -> Vec<&'a Value> {
    all.iter()
        .filter_map(|m| match m {
            Incoming::Notification { method, params } if method == name => Some(params),
            _ => None,
        })
        .collect()
}

fn server_requests<'a>(all: &'a [Incoming], name: &str) -> Vec<(&'a RequestId, &'a Value)> {
    all.iter()
        .filter_map(|m| match m {
            Incoming::ServerRequest { id, method, params } if method == name => Some((id, params)),
            _ => None,
        })
        .collect()
}

#[test]
fn every_sample_is_classified() {
    let all = messages();
    assert_eq!(all.len(), SAMPLES.lines().count());
    assert!(all.iter().all(|m| !matches!(m, Incoming::Invalid { .. })));
}

#[test]
fn turn_start_response_and_lifecycle_parse() {
    let all = messages();
    let resp = all
        .iter()
        .find_map(|m| match m {
            Incoming::Response { result: Ok(v), .. } if v.get("turn").is_some() => Some(v.clone()),
            _ => None,
        })
        .expect("turn/start response");
    let r: TurnStartResult = serde_json::from_value(resp).expect("TurnStartResult");
    assert_eq!(r.turn.status, TurnStatus::InProgress);

    let started: TurnNotification =
        serde_json::from_value(notifications(&all, "turn/started")[0].clone()).expect("started");
    assert_eq!(started.turn.id, r.turn.id);

    let statuses: Vec<TurnStatus> = notifications(&all, "turn/completed")
        .into_iter()
        .map(|p| {
            serde_json::from_value::<TurnNotification>(p.clone())
                .expect("completed")
                .turn
                .status
        })
        .collect();
    assert!(statuses.contains(&TurnStatus::Completed));
    assert!(statuses.contains(&TurnStatus::Interrupted));
    assert!(statuses.contains(&TurnStatus::Failed));
}

#[test]
fn user_message_echoes_client_id() {
    let all = messages();
    let n: ItemNotification =
        serde_json::from_value(notifications(&all, "item/started")[0].clone()).expect("item");
    match n.item {
        ThreadItem::UserMessage { client_id, .. } => {
            assert_eq!(client_id.as_deref(), Some("canterlot-ack-0001"))
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn command_items_carry_exit_codes() {
    let all = messages();
    let codes: Vec<Option<i64>> = notifications(&all, "item/completed")
        .into_iter()
        .filter_map(|p| {
            match serde_json::from_value::<ItemNotification>(p.clone())
                .ok()?
                .item
            {
                ThreadItem::CommandExecution { exit_code, .. } => Some(exit_code),
                _ => None,
            }
        })
        .collect();
    assert!(codes.contains(&Some(0)));
    assert!(codes.contains(&Some(2)));
}

#[test]
fn approvals_bind_to_item_turn_and_thread() {
    let all = messages();
    let (cid, cmd) = server_requests(&all, "item/commandExecution/requestApproval")[0];
    let cmd: CommandApprovalParams = serde_json::from_value(cmd.clone()).expect("cmd approval");
    assert!(
        cmd.command
            .as_deref()
            .is_some_and(|c| c.contains("touch probe.txt"))
    );
    assert!(!cmd.item_id.is_empty() && !cmd.turn_id.is_empty() && !cmd.thread_id.is_empty());

    let (fid, file) = server_requests(&all, "item/fileChange/requestApproval")[0];
    let file: FileChangeApprovalParams =
        serde_json::from_value(file.clone()).expect("file approval");
    assert_eq!(file.turn_id, cmd.turn_id);

    let resolved: Vec<RequestId> = notifications(&all, "serverRequest/resolved")
        .into_iter()
        .map(|p| {
            serde_json::from_value::<ServerRequestResolved>(p.clone())
                .expect("resolved")
                .request_id
        })
        .collect();
    assert!(resolved.contains(cid) && resolved.contains(fid));

    let declined = notifications(&all, "item/completed").into_iter().any(|p| {
        matches!(serde_json::from_value::<ItemNotification>(p.clone()).map(|n| n.item),
            Ok(ThreadItem::FileChange { status, .. }) if status == "declined")
    });
    assert!(declined);
}

#[test]
fn request_user_input_has_options() {
    let all = messages();
    let (_, p) = server_requests(&all, "item/tool/requestUserInput")[0];
    let p: UserInputParams = serde_json::from_value(p.clone()).expect("user input");
    let labels: Vec<String> = p.questions[0]
        .options
        .as_ref()
        .expect("options")
        .iter()
        .map(|o| o.label.clone())
        .collect();
    assert_eq!(labels, vec!["Red", "Blue"]);
    assert!(p.is_blocking);
}

#[test]
fn experimental_rejection_and_errors_parse() {
    let all = messages();
    let rejected = all.iter().any(|m| {
        matches!(m,
        Incoming::Response { result: Err(e), .. } if e.message.contains("requires experimentalApi"))
    });
    assert!(rejected);
    let errors: Vec<ErrorNotification> = notifications(&all, "error")
        .into_iter()
        .map(|p| serde_json::from_value(p.clone()).expect("error notification"))
        .collect();
    assert!(errors.iter().any(|e| e.will_retry));
    let usage: TokenUsageUpdated =
        serde_json::from_value(notifications(&all, "thread/tokenUsage/updated")[0].clone())
            .expect("usage");
    assert!(usage.token_usage["total"]["totalTokens"].as_i64().is_some());
}

#[test]
fn unknown_notifications_are_kept_as_generic() {
    let all = messages();
    assert_eq!(notifications(&all, "turn/moderationMetadata").len(), 1);
}

#[test]
fn multibyte_agent_text_survives_chunked_framing() {
    let all = messages();
    let texts: Vec<String> = notifications(&all, "item/completed")
        .into_iter()
        .filter_map(|p| {
            match serde_json::from_value::<ItemNotification>(p.clone())
                .ok()?
                .item
            {
                ThreadItem::AgentMessage { text, .. } => Some(text),
                _ => None,
            }
        })
        .collect();
    let t = texts
        .iter()
        .find(|t| t.contains('\u{2019}'))
        .expect("text with a right single quotation mark");
    assert!(t.contains("can\u{2019}t"));
    assert!(!t.contains('\u{FFFD}'));
}
