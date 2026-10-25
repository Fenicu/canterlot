//! Структурированные вопросы: AskUserQuestion приходит в permission prompt tool,
//! ответы возвращаются в `updatedInput.answers` (форма из документации canUseTool).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::{Ctx, TURN_TIMEOUT, result_summary};
use crate::broker::{self, PolicyAction};
use crate::config::Config;

pub const FREE_TEXT: &str = "Reykjavik";

/// Ответы по документированному формату: ключ = текст вопроса, значение = label
/// (для multiSelect массив label), для третьего вопроса свободный текст.
pub fn build_answers(input: &Value) -> (Value, Vec<String>) {
    let mut answers = Map::new();
    let mut expected = Vec::new();
    for (i, q) in input["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let text = q["question"].as_str().unwrap_or_default().to_owned();
        let labels: Vec<String> = q["options"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|o| o["label"].as_str().map(str::to_owned))
            .collect();
        let value = match i {
            0 => {
                let pick = labels
                    .get(1)
                    .or(labels.first())
                    .cloned()
                    .unwrap_or_default();
                expected.push(pick.clone());
                Value::String(pick)
            }
            1 => {
                let picks: Vec<String> = [labels.first(), labels.get(2)]
                    .into_iter()
                    .flatten()
                    .cloned()
                    .collect();
                expected.extend(picks.iter().cloned());
                json!(picks)
            }
            _ => {
                expected.push(FREE_TEXT.to_owned());
                Value::String(FREE_TEXT.to_owned())
            }
        };
        answers.insert(text, value);
    }
    (
        json!({"questions": input["questions"], "answers": Value::Object(answers)}),
        expected,
    )
}

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let mut ctx = Ctx::new(cfg, "question", broker::deny_all("placeholder")).await?;
    let expected: Arc<Mutex<Vec<String>>> = Arc::default();
    {
        let expected = expected.clone();
        ctx.broker.set_policy(Arc::new(move |args: &Value| {
            if broker::requested_tool(args) == "AskUserQuestion" {
                let (updated, exp) = build_answers(&broker::requested_input(args));
                if let Ok(mut e) = expected.lock() {
                    *e = exp;
                }
                PolicyAction::Respond(json!({"behavior": "allow", "updatedInput": updated}))
            } else {
                PolicyAction::Respond(broker::deny(
                    "question scenario: only AskUserQuestion is expected",
                ))
            }
        }));
    }
    let mut s = ctx.spawn(&[], &[])?;

    ctx.step("turn: model must ask three structured questions (single, multi, free text) via AskUserQuestion");
    ctx.turns += 1;
    let out = s
        .turn(
            "Before doing anything else, call the AskUserQuestion tool exactly once with these three questions, in this order: \
             (1) question \"Which color do you prefer?\", header \"Color\", options Red, Green, Blue, single select; \
             (2) question \"Which fruits do you like?\", header \"Fruits\", options Apple, Banana, Cherry, multiSelect true; \
             (3) question \"Which city should we use?\", header \"City\", options Paris, Tokyo, single select. \
             After you receive the answers, reply with exactly one line in the form COLOR=<answer>; FRUITS=<answer>; CITY=<answer> using my answers verbatim.",
            TURN_TIMEOUT,
        )
        .await?;
    s.pump_for(Duration::from_secs(1)).await;
    let r = out.result.clone().unwrap_or_default();
    let seen: Vec<_> = std::iter::from_fn(|| ctx.perms.try_recv().ok()).collect();
    let ask = seen
        .iter()
        .find(|p| broker::requested_tool(&p.arguments) == "AskUserQuestion");
    let expected = expected.lock().map(|e| e.clone()).unwrap_or_default();
    let final_text = r["result"].as_str().unwrap_or_default().to_owned();
    let all_present =
        !expected.is_empty() && expected.iter().all(|e| final_text.contains(e.as_str()));

    let ask_use = s
        .tools
        .uses()
        .iter()
        .find(|u| u.name == "AskUserQuestion")
        .cloned();
    let ask_result = ask_use
        .as_ref()
        .and_then(|u| s.tools.result_for(&u.id))
        .cloned();
    let surfaced = ctx.check(
        "question_surfaced_via_permission_tool",
        ask.is_some(),
        json!({"request": ask.map(|p| &p.arguments), "decision": ask.and_then(|p| p.decision.clone()),
               "tool_use": ask_use.as_ref().map(|u| json!({"id": u.id, "input": u.input}))}),
    );
    let received = ctx.check(
        "model_received_answers",
        all_present,
        json!({"expected_answers": expected, "final_text": final_text,
               "ask_tool_result": ask_result.map(|tr| json!({"is_error": tr.is_error, "text": tr.text}))}),
    );
    let structure = ask_use.as_ref().map(|u| {
        u.input["questions"]
            .as_array()
            .map(|qs| qs.iter().map(|q| json!({"multiSelect": q["multiSelect"], "options": q["options"].as_array().map(Vec::len)})).collect::<Vec<_>>())
    });
    ctx.check(
        "question_shapes",
        true,
        json!({"questions": structure, "note": "free text is passed as an answer value that is not one of the option labels (documented 'Other' pattern)"}),
    );
    ctx.check(
        "turn_result",
        r["subtype"] == "success",
        json!({"result": result_summary(&r)}),
    );

    s.p.close_stdin();
    s.pump_for(Duration::from_secs(3)).await;
    let status = s.p.wait_exit(Duration::from_secs(20)).await;
    ctx.rec
        .note(json!({"exit_after_eof": status.map(|st| crate::process::exit_json(&st))}));

    let ok = surfaced && received;
    ctx.finish(
        if ok { "PASS" } else { "FAIL" },
        if ok {
            "AskUserQuestion reached the permission prompt tool; single, multi and free-text answers returned via updatedInput.answers were received by the model"
        } else {
            "see check notes"
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_documented_answer_shape() {
        let input = json!({"questions": [
            {"question": "Q1?", "header": "A", "options": [{"label": "Red"}, {"label": "Green"}], "multiSelect": false},
            {"question": "Q2?", "header": "B", "options": [{"label": "Apple"}, {"label": "Banana"}, {"label": "Cherry"}], "multiSelect": true},
            {"question": "Q3?", "header": "C", "options": [{"label": "Paris"}, {"label": "Tokyo"}], "multiSelect": false}
        ]});
        let (updated, expected) = build_answers(&input);
        assert_eq!(updated["questions"], input["questions"]);
        assert_eq!(updated["answers"]["Q1?"], "Green");
        assert_eq!(updated["answers"]["Q2?"], json!(["Apple", "Cherry"]));
        assert_eq!(updated["answers"]["Q3?"], FREE_TEXT);
        assert_eq!(expected, vec!["Green", "Apple", "Cherry", FREE_TEXT]);
    }
}
