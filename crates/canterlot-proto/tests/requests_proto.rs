use std::collections::BTreeMap;

use canterlot_proto::{
    requests::{AnswerValue, DecisionPayload, PermissionBehavior},
    worker::{ipc, v1::*},
};
use prost::Message;
use serde_json::json;

fn roundtrip<M: Message + Default + PartialEq + std::fmt::Debug>(value: M) {
    assert_eq!(M::decode(value.encode_to_vec().as_slice()).unwrap(), value);
}

fn decision() -> DeliverDecision {
    DeliverDecision {
        request_id: "request".into(),
        decision_id: "decision".into(),
        generation: 42,
        kind: "question".into(),
        decision_payload_json: br#"{"kind":"question","answers":{"q":"Yes"}}"#.to_vec(),
    }
}

#[test]
fn deliver_decision_proto_roundtrip_preserves_generation_and_payload() {
    let value = decision();
    assert_eq!(value.encode_to_vec()[0], 10);
    roundtrip(value.clone());
    let command = Command {
        execution_generation: 42,
        kind: Some(command::Kind::DeliverDecision(value.clone())),
        ..Default::default()
    };
    assert_eq!(&command.encode_to_vec()[2..4], &[0x82, 0x01]);
    roundtrip(command);
    let ipc = ipc::ControlToSupervisor {
        msg: Some(ipc::control_to_supervisor::Msg::DeliverDecision(value)),
    };
    assert_eq!(ipc.encode_to_vec()[0], 11 << 3 | 2);
    roundtrip(ipc);
    let ack = ipc::SupervisorToControl {
        msg: Some(ipc::supervisor_to_control::Msg::DecisionAck(
            ipc::DecisionAck {
                request_id: "request".into(),
                decision_id: "decision".into(),
                result: "stored".into(),
            },
        )),
    };
    assert_eq!(ack.encode_to_vec()[0], 7 << 3 | 2);
    roundtrip(ack);
    for outcome in [
        ipc::decision_outcome::Outcome::Applied(DecisionApplied {
            request_id: "request".into(),
            decision_id: "decision".into(),
            proof_kind: "tool_result".into(),
            proof_payload_json: br#"{"result":"ok"}"#.to_vec(),
        }),
        ipc::decision_outcome::Outcome::NotApplied(DecisionNotApplied {
            request_id: "request".into(),
            decision_id: "decision".into(),
            reason: "stale_generation".into(),
        }),
        ipc::decision_outcome::Outcome::Unconfirmed(DecisionUnconfirmed {
            request_id: "request".into(),
            decision_id: "decision".into(),
            reason: "proof_missing".into(),
        }),
    ] {
        let ipc = ipc::SupervisorToControl {
            msg: Some(ipc::supervisor_to_control::Msg::DecisionOutcome(
                ipc::DecisionOutcome {
                    outcome: Some(outcome),
                },
            )),
        };
        assert_eq!(ipc.encode_to_vec()[0], 8 << 3 | 2);
        roundtrip(ipc);
    }
}

#[test]
fn record_body_request_variants_serialize_and_deserialize() {
    use record_body::Body;
    for (tag, body) in [
        (
            28,
            Body::RequestOpened(RequestOpened {
                request_id: "r".into(),
                execution_id: "e".into(),
                generation: 42,
                kind: "permission".into(),
                payload_json: br#"{"tool_name":"shell","input":{"command":"pwd"}}"#.to_vec(),
            }),
        ),
        (
            29,
            Body::RequestClosed(RequestClosed {
                request_id: "r".into(),
                reason: "interrupted".into(),
            }),
        ),
        (
            30,
            Body::DecisionTransmissionStarted(DecisionTransmissionStarted {
                request_id: "r".into(),
                decision_id: "d".into(),
                machine_time_ms: 123,
            }),
        ),
        (
            31,
            Body::DecisionApplied(DecisionApplied {
                request_id: "r".into(),
                decision_id: "d".into(),
                proof_kind: "tool_result".into(),
                proof_payload_json: br#"{"ok":true}"#.to_vec(),
            }),
        ),
        (
            32,
            Body::DecisionNotApplied(DecisionNotApplied {
                request_id: "r".into(),
                decision_id: "d".into(),
                reason: "already_transmitted".into(),
            }),
        ),
        (
            33,
            Body::DecisionUnconfirmed(DecisionUnconfirmed {
                request_id: "r".into(),
                decision_id: "d".into(),
                reason: "proof_missing".into(),
            }),
        ),
    ] {
        let body = RecordBody { body: Some(body) };
        let key = (tag << 3) | 2;
        assert_eq!(
            &body.encode_to_vec()[..2],
            &[(key as u8 & 0x7f) | 0x80, (key >> 7) as u8]
        );
        roundtrip(body.clone());
        roundtrip(JournalRecord {
            seq: 7,
            body: Some(body),
        });
    }
}

#[test]
fn decision_payload_json_schema_matches_discriminated_union() {
    for behavior in [PermissionBehavior::Allow, PermissionBehavior::Deny] {
        let value = DecisionPayload::Permission {
            behavior: behavior.clone(),
            message: Some("Причина".into()),
        };
        let expected = json!({"kind":"permission", "behavior": if behavior == PermissionBehavior::Allow {"allow"} else {"deny"}, "message":"Причина"});
        assert_eq!(serde_json::to_value(&value).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<DecisionPayload>(expected).unwrap(),
            value
        );
    }
    let value = DecisionPayload::Question {
        answers: BTreeMap::from([
            ("q1".into(), AnswerValue::Single("Raw Label".into())),
            (
                "q2".into(),
                AnswerValue::Multiple(vec!["A".into(), "Б".into()]),
            ),
            ("q3".into(), AnswerValue::Single("Свой ответ".into())),
            ("q4".into(), AnswerValue::Multiple(vec![])),
        ]),
    };
    let expected = json!({"kind":"question", "answers":{"q1":"Raw Label", "q2":["A","Б"], "q3":"Свой ответ", "q4":[]}});
    assert_eq!(serde_json::to_value(&value).unwrap(), expected);
    assert_eq!(
        serde_json::from_value::<DecisionPayload>(expected).unwrap(),
        value
    );
    for invalid in [
        json!({"kind":"other"}),
        json!({"kind":"permission","behavior":"accept"}),
        json!({"kind":"question","answers":{"q":true}}),
        json!({"kind":"question","answers":{"q":[1]}}),
    ] {
        assert!(serde_json::from_value::<DecisionPayload>(invalid).is_err());
    }
}

#[test]
fn redacted_debug_hides_decision_messages_and_answers() {
    let marker = "PRIVATE_REQUEST_MARKER";
    let values = [
        AnswerValue::Single(marker.into()),
        AnswerValue::Multiple(vec![marker.into()]),
    ];
    let mut debug = values.iter().map(|v| format!("{v:?}")).collect::<Vec<_>>();
    debug.push(format!(
        "{:?}",
        DecisionPayload::Permission {
            behavior: PermissionBehavior::Deny,
            message: Some(marker.into())
        }
    ));
    debug.push(format!(
        "{:?}",
        DecisionPayload::Question {
            answers: BTreeMap::from([(marker.into(), values[0].clone())])
        }
    ));
    let deliver = DeliverDecision {
        decision_payload_json: marker.as_bytes().to_vec(),
        ..decision()
    };
    debug.push(format!("{deliver:?}"));
    debug.push(format!(
        "{:?}",
        Command {
            kind: Some(command::Kind::DeliverDecision(deliver.clone())),
            ..Default::default()
        }
    ));
    debug.push(format!(
        "{:?}",
        ipc::ControlToSupervisor {
            msg: Some(ipc::control_to_supervisor::Msg::DeliverDecision(deliver))
        }
    ));
    let opened = RequestOpened {
        payload_json: marker.as_bytes().to_vec(),
        ..Default::default()
    };
    debug.push(format!("{opened:?}"));
    debug.push(format!(
        "{:?}",
        JournalRecord {
            seq: 1,
            body: Some(RecordBody {
                body: Some(record_body::Body::RequestOpened(opened))
            })
        }
    ));
    let applied = DecisionApplied {
        proof_payload_json: marker.as_bytes().to_vec(),
        ..Default::default()
    };
    debug.push(format!("{applied:?}"));
    debug.push(format!(
        "{:?}",
        ipc::SupervisorToControl {
            msg: Some(ipc::supervisor_to_control::Msg::DecisionOutcome(
                ipc::DecisionOutcome {
                    outcome: Some(ipc::decision_outcome::Outcome::Applied(applied))
                }
            ))
        }
    ));
    for value in debug {
        assert!(!value.contains(marker));
        assert!(!value.contains(&format!("{:?}", marker.as_bytes())));
        assert!(value.contains("[REDACTED]"));
    }
}
