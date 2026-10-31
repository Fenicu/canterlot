#![forbid(unsafe_code)]

//! Ввод Claude Code (план 2a, задача 3): очередь по одному ходу, сообщения
//! пользователя с вложениями, доказательство доставки, смена настроек между
//! ходами и прерывание SIGINT. Записанные кадры берутся из
//! docs/stage0/evidence/claude-code/, процесс CLI заменяет скрипт bash.

use canterlot_worker::adapter::{
    AdapterEvent, InstructionMode, InstructionSpec, InterruptOutcome, LocalFile, SettingsChange,
    Transmission, TransmitOutcome,
    claude_code::{
        input::{InputDriver, InputQueue, LaunchSettings, Next, user_message},
        parser::{Parsed, Parser},
        process::{CliProcess, LaunchSpec, SessionArg, StdoutLine, exit_event},
    },
};
use canterlot_worker::config::ClaudeCodeConfig;
use canterlot_worker::containment::TreeContainment;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

#[path = "support/fake_exec.rs"]
mod fake_exec;

const WAIT: Duration = Duration::from_secs(10);

fn owner_instructions() -> InstructionSpec {
    InstructionSpec {
        mode: InstructionMode::Owner,
        text: String::new(),
        preset_id: None,
        revision: 0,
        title: String::new(),
    }
}
const DEADLINE: Duration = Duration::from_secs(10);

const ALPHA: &str = "1c8c60ee-df06-42e6-b98a-2d9597113db7";
const BRAVO: &str = "b0d18838-2e43-4915-b270-75a29fc34d78";
const SLEEP_INPUT: &str = "0273ad61-0fb3-4d86-80e0-8491df21103e";

fn id(text: &str) -> Uuid {
    Uuid::parse_str(text).unwrap()
}

fn evidence(name: &str) -> Vec<(usize, String, Value)> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/stage0/evidence/claude-code")
        .join(format!("{name}.jsonl"));
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .enumerate()
        .map(|(index, line)| {
            let wrapper: Value = serde_json::from_str(line).unwrap();
            (
                index + 1,
                wrapper["dir"].as_str().unwrap().to_owned(),
                wrapper["data"].clone(),
            )
        })
        .collect()
}

/// `out`-кадры файла доказательств в строках `from..=to`, по одному JSON на строку.
fn out_lines(name: &str, from: usize, to: usize) -> Vec<(usize, String)> {
    evidence(name)
        .into_iter()
        .filter(|(line, dir, _)| dir == "out" && (from..=to).contains(line))
        .map(|(line, _, data)| (line, serde_json::to_string(&data).unwrap()))
        .collect()
}

fn in_line(name: &str, line: usize) -> Value {
    let (_, dir, data) = evidence(name)
        .into_iter()
        .find(|(number, ..)| *number == line)
        .unwrap();
    assert_eq!(dir, "in");
    data
}

fn launch(permission_mode: &str) -> LaunchSettings {
    LaunchSettings {
        model: "haiku".into(),
        effort: "low".into(),
        permission_mode: permission_mode.into(),
    }
}

fn tx(command_id: Uuid, attempt: u32, text: &str) -> Transmission {
    Transmission {
        command_id,
        attempt,
        text: text.into(),
        files: Vec::new(),
        settings: None,
    }
}

fn with_settings(mut t: Transmission, change: SettingsChange) -> Transmission {
    t.settings = Some(change);
    t
}

fn model(name: &str) -> SettingsChange {
    SettingsChange {
        model: Some(name.into()),
        ..Default::default()
    }
}

fn mode(name: &str) -> SettingsChange {
    SettingsChange {
        permission_mode: Some(name.into()),
        ..Default::default()
    }
}

fn written(next: Next) -> Value {
    match next {
        Next::Write(value) => value,
        Next::Idle => panic!("expected a stdin line, got Idle"),
    }
}

fn run_finished(command_id: Uuid) -> Parsed {
    Parsed::Event(AdapterEvent::RunFinished {
        input_command_id: command_id,
        usage: Vec::new(),
        context_tokens: 0,
        context_window_tokens: 0,
    })
}

// --- поддельный CLI -----------------------------------------------------------

/// Скрипт bash вместо `claude`: пишет каждую строку stdin в `stdin.log`,
/// на n-й ввод пользователя печатает `turn_<n>.out`, на управляющий запрос
/// отвечает по файлу `control` (`success` по умолчанию, `error`, `none`).
/// Строка `@probe` в кадрах ждёт 0,4 с и отмечает в `events.log` ранний ввод,
/// `@wait` ждёт SIGINT встроенным `read` (без дочерних процессов: SIGINT во время
/// ожидания `sleep` на переднем плане зацикливал bash); по SIGINT печатается
/// `int.out` и процесс выходит с 0, по закрытию stdin просто выходит.
const FAKE_CLI: &str = r#"#!/bin/bash
dir="$(cd "$(dirname "$0")" && pwd)"
n=0
held=""
emit() {
  while IFS= read -r frame <&3; do
    case "$frame" in
      @probe) if IFS= read -r -t 0.4 early; then echo EARLY >> "$dir/events.log"; held="$early"; fi ;;
      @wait) IFS= read -r _ || exit 0 ;;
      *) printf '%s\n' "$frame" ;;
    esac
  done 3< "$1"
}
trap 'if [ -f "$dir/int.out" ]; then emit "$dir/int.out"; fi; exit 0' INT
while :; do
  if [ -n "$held" ]; then line="$held"; held=""
  elif ! IFS= read -r line; then break
  fi
  printf '%s\n' "$line" >> "$dir/stdin.log"
  case "$line" in
    *'"type":"control_request"'*)
      rid="$(printf '%s' "$line" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p')"
      reply="$(cat "$dir/control" 2>/dev/null || echo success)"
      case "$reply" in
        success) printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$rid" ;;
        error) printf '{"type":"control_response","response":{"subtype":"error","request_id":"%s","error":"refused by fake"}}\n' "$rid" ;;
      esac ;;
    *)
      n=$((n+1))
      if [ -f "$dir/turn_$n.out" ]; then emit "$dir/turn_$n.out"; fi ;;
  esac
done
"#;

struct Fake {
    dir: TempDir,
    _group: TreeContainment,
    cli: CliProcess,
}

impl Fake {
    async fn start(setup: impl FnOnce(&Path)) -> Self {
        let dir = TempDir::new().unwrap();
        let script = dir.path().join("claude");
        fake_exec::write_executable(&script, FAKE_CLI, 0o755);
        setup(dir.path());
        let group = TreeContainment::new().unwrap();
        let spec = LaunchSpec {
            executable: &script,
            exec_dir: dir.path(),
            workdir: dir.path(),
            session: SessionArg::New(Uuid::nil()),
            model: "haiku",
            effort: "low",
            permission_mode: "ask",
            instructions: &owner_instructions(),
        };
        let cli = CliProcess::spawn(&spec, &ClaudeCodeConfig::default(), &group)
            .await
            .unwrap();
        Self {
            dir,
            _group: group,
            cli,
        }
    }

    fn stdin_log(&self) -> Vec<Value> {
        std::fs::read_to_string(self.dir.path().join("stdin.log"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn early_writes(&self) -> bool {
        self.dir.path().join("events.log").exists()
    }

    fn control(&self, reply: &str) {
        std::fs::write(self.dir.path().join("control"), reply).unwrap();
    }
}

fn write_frames(dir: &Path, file: &str, frames: &[String]) {
    std::fs::write(dir.join(file), frames.join("\n") + "\n").unwrap();
}

/// Кадры первого хода `multi_input.jsonl` (строки 5–26) с UUID ввода `command`;
/// перед `result` стоит `@probe`.
fn alpha_turn(command: Uuid) -> Vec<String> {
    let mut frames: Vec<String> = out_lines("multi_input", 5, 26)
        .into_iter()
        .map(|(_, frame)| frame.replace(ALPHA, &command.to_string()))
        .collect();
    let result = frames.pop().unwrap();
    frames.push("@probe".into());
    frames.push(result);
    frames
}

async fn finish_turn(driver: &mut InputDriver, fake: &mut Fake) {
    tokio::time::timeout(WAIT, async {
        while driver.queue.active().is_some() {
            assert!(!driver.eof(), "CLI ended inside a turn");
            driver.pump(&mut fake.cli).await;
        }
    })
    .await
    .expect("turn did not end");
}

fn drain(driver: &mut InputDriver) -> Vec<Parsed> {
    std::iter::from_fn(|| driver.next_parsed()).collect()
}

// --- очередь и доставка ---------------------------------------------------------

#[tokio::test]
async fn second_input_waits_for_first_result() {
    let mut fake = Fake::start(|dir| {
        let mut first: Vec<String> = out_lines("multi_input", 5, 26)
            .into_iter()
            .map(|(_, frame)| frame)
            .collect();
        let result = first.pop().unwrap();
        first.push("@probe".into());
        first.push(result);
        write_frames(dir, "turn_1.out", &first);
        let second: Vec<String> = out_lines("multi_input", 31, 44)
            .into_iter()
            .map(|(_, frame)| frame)
            .collect();
        write_frames(dir, "turn_2.out", &second);
    })
    .await;
    let mut driver = InputDriver::new(InputQueue::new(launch("ask")), Parser::new());
    let alpha = tx(
        id(ALPHA),
        1,
        "Reply with exactly the single word ALPHA and nothing else.",
    );
    let bravo = tx(
        id(BRAVO),
        1,
        "Reply with exactly the single word BRAVO and nothing else.",
    );

    let first = tokio::time::timeout(WAIT, driver.transmit(&mut fake.cli, &alpha, None, DEADLINE))
        .await
        .unwrap();
    assert!(matches!(first, TransmitOutcome::Accepted(ref p) if p.command_id == id(ALPHA)));
    assert!(driver.queue.active().is_some(), "first turn still running");

    // Второй ввод отдан, пока первый ход идёт: в stdin он попадает только после `result`.
    let second = tokio::time::timeout(WAIT, driver.transmit(&mut fake.cli, &bravo, None, DEADLINE))
        .await
        .unwrap();
    assert!(matches!(second, TransmitOutcome::Accepted(ref p) if p.command_id == id(BRAVO)));
    assert!(
        !fake.early_writes(),
        "nothing may be written to stdin while a turn is active"
    );
    let parsed = drain(&mut driver);
    let finished: Vec<Uuid> = parsed
        .iter()
        .filter_map(|p| match p {
            Parsed::Event(AdapterEvent::RunFinished {
                input_command_id, ..
            }) => Some(*input_command_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        finished,
        vec![id(ALPHA)],
        "first turn ended before the second was written"
    );
    let uuids: Vec<String> = fake
        .stdin_log()
        .iter()
        .map(|line| line["uuid"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(uuids, vec![ALPHA.to_owned(), BRAVO.to_owned()]);
}

/// Строки записанной фикстуры `from..=to` без обёртки; `in` пропускаются.
fn fixture_frames(name: &str, from: usize, to: usize) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/claude_code")
        .join(format!("{name}.jsonl"));
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|row| row["dir"] != "note")
        .enumerate()
        .filter(|(index, row)| (from..=to).contains(index) && row["dir"] == "out")
        .map(|(_, row)| serde_json::to_string(&row["data"]).unwrap())
        .collect()
}

// Порядок записанного кадра за кадром: сообщение записано на уведомлении о завершении,
// пробуждающий ход начинается следом, его `result` приходит раньше ответа на сообщение.
#[tokio::test]
async fn message_written_during_wake_turn_is_still_proven_by_its_own_answer_frame() {
    const FIRST: &str = "72e9efaa-797a-4bec-90f3-ac637db9a507";
    const SECOND: &str = "346dc38a-f607-470c-b440-a96c9ae86d09";
    let rows = fixture_frames("background_race", 0, 1000);
    let split = rows
        .iter()
        .position(|frame| frame.contains("\"subtype\":\"task_notification\""))
        .unwrap();
    let last = rows
        .iter()
        .rposition(|frame| frame.contains(&format!("\"user_message_uuid\":\"{SECOND}\"")))
        .unwrap();
    let mut fake = Fake::start(|dir| {
        write_frames(dir, "turn_1.out", &rows[..=split]);
        write_frames(dir, "turn_2.out", &rows[split + 1..=last]);
    })
    .await;
    let mut driver = InputDriver::new(InputQueue::new(launch("ask")), Parser::new());
    let first = tokio::time::timeout(
        WAIT,
        driver.transmit(&mut fake.cli, &tx(id(FIRST), 1, "one"), None, DEADLINE),
    )
    .await
    .unwrap();
    assert!(matches!(first, TransmitOutcome::Accepted(_)));
    finish_turn(&mut driver, &mut fake).await;
    drain(&mut driver);

    let second = tokio::time::timeout(
        WAIT,
        driver.transmit(&mut fake.cli, &tx(id(SECOND), 1, "two"), None, DEADLINE),
    )
    .await
    .unwrap();
    assert!(
        matches!(second, TransmitOutcome::Accepted(ref p) if p.command_id == id(SECOND)),
        "proven by its own answer frame, got {second:?}"
    );
    finish_turn(&mut driver, &mut fake).await;
    let finished: Vec<Uuid> = drain(&mut driver)
        .iter()
        .filter_map(|p| match p {
            Parsed::Event(AdapterEvent::RunFinished {
                input_command_id, ..
            }) => Some(*input_command_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        finished,
        vec![id(SECOND)],
        "one finish, from its own result, no early one from the wake result"
    );
}

#[test]
fn delivery_is_proven_by_answer_frame_not_by_echo() {
    let mut queue = InputQueue::new(launch("ask"));
    let mut parser = Parser::new();
    queue.enqueue(&tx(
        id(ALPHA),
        1,
        "Reply with exactly the single word ALPHA and nothing else.",
    ));
    written(queue.next(None));
    parser.set_input_command(id(ALPHA));
    let mut proofs = Vec::new();
    let mut echo_lines = Vec::new();
    for (line, frame) in out_lines("multi_input", 5, 26) {
        for parsed in parser.feed(&frame) {
            if matches!(parsed, Parsed::Replay { .. }) {
                echo_lines.push(line);
            }
            if let Some(proof) = queue.on_parsed(&parsed) {
                proofs.push((line, proof));
            }
        }
        if line <= 17 {
            assert!(
                proofs.is_empty(),
                "line {line}: system frames and the replay echo prove nothing"
            );
            assert!(!queue.is_proven(id(ALPHA)));
        }
    }
    assert_eq!(echo_lines, vec![17]);
    assert_eq!(proofs.len(), 1, "one proof per input");
    let (line, proof) = &proofs[0];
    assert_eq!(
        *line, 18,
        "the first stream_event after the echo proves delivery"
    );
    assert_eq!(proof.command_id, id(ALPHA));
    assert_eq!(proof.kind, "claude_code_answer_frame");
    assert!(queue.is_proven(id(ALPHA)));
    assert!(queue.active().is_none(), "the result ended the turn");
}

#[tokio::test]
async fn retry_reuses_uuid_and_is_not_resent_after_proof() {
    let command = Uuid::now_v7();
    let mut first = InputQueue::new(launch("ask"));
    first.enqueue(&tx(command, 1, "hello"));
    let original = written(first.next(None));
    assert_eq!(original["uuid"], command.to_string());

    // Повтор в новом исполнении: тот же uuid и те же байты.
    let mut second = InputQueue::new(launch("ask"));
    second.enqueue(&tx(command, 2, "hello"));
    let retried = written(second.next(None));
    assert_eq!(retried, original);
    assert!(
        second
            .on_parsed(&Parsed::AnswerFrame {
                user_message_uuids: vec![command],
            })
            .is_some()
    );
    second.enqueue(&tx(command, 3, "hello"));
    assert!(second.is_proven(command));
    assert!(matches!(second.next(None), Next::Idle));
    second.on_parsed(&run_finished(command));
    assert!(
        matches!(second.next(None), Next::Idle),
        "a proven input is never written again"
    );

    // Через процесс: повтор доказанного ввода принимается без записи в stdin.
    let mut fake = Fake::start(|dir| write_frames(dir, "turn_1.out", &alpha_turn(command))).await;
    let mut driver = InputDriver::new(InputQueue::new(launch("ask")), Parser::new());
    let outcome = driver
        .transmit(&mut fake.cli, &tx(command, 1, "hello"), None, DEADLINE)
        .await;
    assert!(matches!(outcome, TransmitOutcome::Accepted(_)));
    finish_turn(&mut driver, &mut fake).await;
    let retry = tokio::time::timeout(
        WAIT,
        driver.transmit(&mut fake.cli, &tx(command, 2, "hello"), None, DEADLINE),
    )
    .await
    .unwrap();
    assert!(matches!(retry, TransmitOutcome::Accepted(ref p) if p.command_id == command));
    assert_eq!(fake.stdin_log().len(), 1, "the retry was not re-sent");
}

#[test]
fn message_bytes_match_recorded_stdin() {
    let recorded = in_line("multi_input", 4);
    let text = recorded["message"]["content"].as_str().unwrap();
    let command = Uuid::now_v7();
    let t = tx(command, 1, text);
    let mut ours = user_message(&t, None).unwrap();
    assert_eq!(ours["uuid"], command.to_string());
    ours["uuid"] = recorded["uuid"].clone();
    assert_eq!(
        serde_json::to_string(&ours).unwrap(),
        serde_json::to_string(&recorded).unwrap()
    );

    let mut queue = InputQueue::new(launch("ask"));
    queue.enqueue(&t);
    let mut line = written(queue.next(None));
    line["uuid"] = recorded["uuid"].clone();
    assert_eq!(line, recorded, "the queue writes the same line");
}

#[test]
fn attachments_add_paths_and_small_images_as_native_blocks() {
    use base64::Engine;
    let recorded = in_line("image_input", 4);
    // В доказательствах данные картинки обрезаны: форму блока берём оттуда, байты свои.
    let png: Vec<u8> = b"\x89PNG\r\n\x1a\n"
        .iter()
        .copied()
        .chain((0..200u8).map(|byte| byte.wrapping_mul(7)))
        .collect();
    let mut image_block = recorded["message"]["content"][0].clone();
    image_block["source"]["data"] = json!(base64::engine::general_purpose::STANDARD.encode(&png));
    let text_block = recorded["message"]["content"][1].clone();

    let dir = TempDir::new().unwrap();
    let small = dir.path().join("red.png");
    std::fs::write(&small, &png).unwrap();
    let big = dir.path().join("big.png");
    let mut big_bytes = png.clone();
    big_bytes.resize(6 * 1024 * 1024, 0);
    std::fs::write(&big, &big_bytes).unwrap();
    let pdf = dir.path().join("doc.pdf");
    std::fs::write(&pdf, b"%PDF-1.4\n%%EOF\n").unwrap();
    let file = |path: &Path, mime: &str, image: bool| LocalFile {
        path: path.to_owned(),
        name: path.file_name().unwrap().to_string_lossy().into_owned(),
        mime: mime.into(),
        size: std::fs::metadata(path).unwrap().len(),
        image,
    };
    let mut t = tx(Uuid::now_v7(), 1, text_block["text"].as_str().unwrap());
    t.files = vec![
        file(&small, "image/png", true),
        file(&big, "image/png", true),
        file(&pdf, "application/pdf", false),
    ];
    let message = user_message(&t, None).unwrap();
    let content = message["message"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 3, "small image, owner text, attachment list");
    assert_eq!(
        content[0], image_block,
        "image block as recorded, image first"
    );
    assert_eq!(content[1], text_block, "owner text after the images");
    assert_eq!(
        content
            .iter()
            .filter(|block| block["type"] == "image")
            .count(),
        1,
        "the 6 MiB PNG and the PDF go by path only"
    );
    assert_eq!(content[2]["type"], "text");
    let list = content[2]["text"].as_str().unwrap();
    for (path, mime) in [
        (&small, "image/png"),
        (&big, "image/png"),
        (&pdf, "application/pdf"),
    ] {
        let line = format!(
            "{} ({mime}, {} bytes): {}",
            path.file_name().unwrap().to_string_lossy(),
            std::fs::metadata(path).unwrap().len(),
            path.display()
        );
        assert!(list.contains(&line), "missing attachment line {line:?}");
    }
}

// --- настройки посреди сессии ----------------------------------------------------

#[tokio::test]
async fn one_million_variant_goes_to_set_model_unchanged() {
    let (one, two) = (Uuid::now_v7(), Uuid::now_v7());
    let mut fake = Fake::start(|dir| {
        write_frames(dir, "turn_1.out", &alpha_turn(one));
        write_frames(dir, "turn_2.out", &alpha_turn(two));
    })
    .await;
    let mut driver = InputDriver::new(
        InputQueue::new(launch("ask")).with_control_timeout(Duration::from_secs(1)),
        Parser::new(),
    );
    let outcome = driver
        .transmit(&mut fake.cli, &tx(one, 1, "one"), None, DEADLINE)
        .await;
    assert!(matches!(outcome, TransmitOutcome::Accepted(_)));
    let outcome = tokio::time::timeout(
        WAIT,
        driver.transmit(
            &mut fake.cli,
            &with_settings(tx(two, 1, "two"), model("fable[1m]")),
            None,
            DEADLINE,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, TransmitOutcome::Accepted(_)));
    assert_eq!(
        fake.stdin_log()[1]["request"],
        json!({"subtype": "set_model", "model": "fable[1m]"})
    );
    finish_turn(&mut driver, &mut fake).await;
    assert_eq!(driver.queue.effective().model, "fable[1m]");
}

#[tokio::test]
async fn model_change_sends_set_model_between_turns_and_rejects_the_input_when_it_fails() {
    let (one, two, three, four, five) = (
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
        Uuid::now_v7(),
    );
    let mut fake = Fake::start(|dir| {
        write_frames(dir, "turn_1.out", &alpha_turn(one));
        write_frames(dir, "turn_2.out", &alpha_turn(two));
        write_frames(dir, "turn_3.out", &alpha_turn(five));
    })
    .await;
    let mut driver = InputDriver::new(
        InputQueue::new(launch("ask")).with_control_timeout(Duration::from_secs(1)),
        Parser::new(),
    );
    let outcome = driver
        .transmit(&mut fake.cli, &tx(one, 1, "one"), None, DEADLINE)
        .await;
    assert!(matches!(outcome, TransmitOutcome::Accepted(_)));

    // Смена модели отдана посреди хода: управляющий запрос уходит только после `result`.
    let outcome = tokio::time::timeout(
        WAIT,
        driver.transmit(
            &mut fake.cli,
            &with_settings(tx(two, 1, "two"), model("sonnet")),
            None,
            DEADLINE,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, TransmitOutcome::Accepted(ref p) if p.command_id == two));
    assert!(!fake.early_writes(), "set_model waited for the turn to end");
    let log = fake.stdin_log();
    assert_eq!(log.len(), 3);
    assert_eq!(log[0]["uuid"], one.to_string());
    assert_eq!(log[1]["type"], "control_request");
    assert_eq!(
        log[1]["request"],
        json!({"subtype": "set_model", "model": "sonnet"})
    );
    assert!(!log[1]["request_id"].as_str().unwrap().is_empty());
    assert_eq!(log[2]["uuid"], two.to_string());
    assert_eq!(driver.queue.effective().model, "sonnet");
    finish_turn(&mut driver, &mut fake).await;

    // Отказ CLI: ввод не отправлен, исполнитель видит отказ.
    fake.control("error");
    let outcome = tokio::time::timeout(
        WAIT,
        driver.transmit(
            &mut fake.cli,
            &with_settings(tx(three, 1, "three"), model("opus")),
            None,
            DEADLINE,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, TransmitOutcome::Rejected(ref r) if r == "model_change_failed"));

    // Нет ответа за отведённое время: тот же отказ.
    fake.control("none");
    let outcome = tokio::time::timeout(
        WAIT,
        driver.transmit(
            &mut fake.cli,
            &with_settings(tx(four, 1, "four"), model("opus")),
            None,
            DEADLINE,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, TransmitOutcome::Rejected(ref r) if r == "model_change_failed"));
    assert_eq!(
        driver.queue.effective().model,
        "sonnet",
        "no silent fallback"
    );

    let outcome = tokio::time::timeout(
        WAIT,
        driver.transmit(&mut fake.cli, &tx(five, 1, "five"), None, DEADLINE),
    )
    .await
    .unwrap();
    assert!(
        matches!(outcome, TransmitOutcome::Accepted(_)),
        "the session goes on"
    );
    let log = fake.stdin_log();
    let users: Vec<&str> = log
        .iter()
        .filter(|line| line["type"] == "user")
        .map(|line| line["uuid"].as_str().unwrap())
        .collect();
    assert_eq!(
        users,
        vec![one.to_string(), two.to_string(), five.to_string()],
        "rejected inputs never reach stdin"
    );
    assert_eq!(
        log.iter()
            .filter(|line| line["type"] == "control_request")
            .count(),
        3
    );
}

#[test]
fn effort_change_is_rejected_until_verified() {
    let mut queue = InputQueue::new(launch("ask"));
    let change = SettingsChange {
        effort: Some("high".into()),
        ..Default::default()
    };
    assert_eq!(
        queue.validate(Some(&change)),
        Err("effort_change_unsupported")
    );
    let same = SettingsChange {
        effort: Some("low".into()),
        ..Default::default()
    };
    assert_eq!(
        queue.validate(Some(&same)),
        Ok(()),
        "unchanged effort is no change"
    );

    let command = Uuid::now_v7();
    queue.enqueue(&with_settings(tx(command, 1, "go"), change));
    assert!(matches!(queue.next(None), Next::Idle), "nothing is written");
    assert_eq!(
        queue.take_rejection(command),
        Some("effort_change_unsupported")
    );
}

#[test]
fn switching_into_bypass_is_refused_without_bypass_launch() {
    let mut ask = InputQueue::new(launch("ask"));
    assert_eq!(
        ask.validate(Some(&mode("bypass"))),
        Err("permission_mode_needs_restart")
    );
    assert_eq!(
        ask.validate(Some(&mode("owner"))),
        Err("permission_mode_needs_restart"),
        "the owner's default mode is unknown to a session launched in another mode"
    );
    assert_eq!(ask.validate(Some(&mode("read_only"))), Ok(()));
    ask.on_parsed(&Parsed::Init {
        session_id: "s".into(),
        model: "claude-haiku-5-5".into(),
        permission_mode: "default".into(),
        version: "2.1.311".into(),
    });
    assert_eq!(
        ask.validate(Some(&mode("bypass"))),
        Err("permission_mode_needs_restart")
    );

    let command = Uuid::now_v7();
    ask.enqueue(&with_settings(tx(command, 1, "plan it"), mode("read_only")));
    let control = written(ask.next(None));
    assert_eq!(control["type"], "control_request");
    assert_eq!(
        control["request"],
        json!({"subtype": "set_permission_mode", "mode": "plan"})
    );
    assert!(
        matches!(ask.next(None), Next::Idle),
        "waits for control_response"
    );
    ask.on_parsed(&Parsed::ControlResponse {
        request_id: control["request_id"].as_str().unwrap().into(),
        success: true,
    });
    let message = written(ask.next(None));
    assert_eq!(message["uuid"], command.to_string());
    assert_eq!(ask.effective().permission_mode, "read_only");

    // Запуск в bypass: туда и обратно можно.
    let mut bypass = InputQueue::new(launch("bypass"));
    let back = Uuid::now_v7();
    bypass.enqueue(&with_settings(tx(back, 1, "ask me"), mode("ask")));
    let control = written(bypass.next(None));
    assert_eq!(
        control["request"],
        json!({"subtype": "set_permission_mode", "mode": "default"})
    );
    bypass.on_parsed(&Parsed::ControlResponse {
        request_id: control["request_id"].as_str().unwrap().into(),
        success: true,
    });
    written(bypass.next(None));
    bypass.on_parsed(&run_finished(back));
    assert_eq!(bypass.validate(Some(&mode("bypass"))), Ok(()));

    // Режим владельца, который сам оказался bypassPermissions, тоже допускает bypass.
    let mut owner = InputQueue::new(launch("owner"));
    owner.on_parsed(&Parsed::Init {
        session_id: "s".into(),
        model: "claude-haiku-5-5".into(),
        permission_mode: "bypassPermissions".into(),
        version: "2.1.311".into(),
    });
    assert_eq!(owner.validate(Some(&mode("bypass"))), Ok(()));
}

#[test]
fn weekly_threshold_note_is_added_once_per_threshold() {
    let mut queue = InputQueue::new(launch("ask"));
    let mut send = |weekly: Option<(u16, Option<u64>)>| -> Value {
        let command = Uuid::now_v7();
        queue.enqueue(&tx(command, 1, "owner text"));
        let message = written(queue.next(weekly));
        queue.on_parsed(&run_finished(command));
        message["message"]["content"].clone()
    };
    let note = |content: &Value| -> Option<String> {
        let first = content.as_array()?.first()?;
        let text = first["text"].as_str()?;
        text.starts_with("[canterlot]").then(|| text.to_owned())
    };
    let reset_ms: u64 = 1_793_062_800_000;
    let reset = chrono::DateTime::from_timestamp_millis(reset_ms as i64)
        .unwrap()
        .with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M %:z")
        .to_string();

    assert_eq!(send(None), json!("owner text"), "no data, plain string");
    assert_eq!(send(Some((790, Some(reset_ms)))), json!("owner text"));
    let crossed = send(Some((805, Some(reset_ms))));
    assert_eq!(
        note(&crossed).as_deref(),
        Some(format!("[canterlot] Weekly Claude limit: 80 % used, resets {reset}.").as_str())
    );
    assert_eq!(crossed[1], json!({"type": "text", "text": "owner text"}));
    assert_eq!(send(Some((850, Some(reset_ms)))), json!("owner text"));
    assert_eq!(
        note(&send(Some((905, None)))).as_deref(),
        Some("[canterlot] Weekly Claude limit: 90 % used.")
    );
    assert_eq!(send(Some((910, Some(reset_ms)))), json!("owner text"));
    assert_eq!(
        note(&send(Some((960, Some(reset_ms))))).as_deref(),
        Some(format!("[canterlot] Weekly Claude limit: 96 % used, resets {reset}.").as_str())
    );
    assert_eq!(send(Some((990, Some(reset_ms)))), json!("owner text"));
    assert_eq!(
        send(Some((100, Some(reset_ms)))),
        json!("owner text"),
        "after a reset the window drops"
    );
    assert_eq!(
        send(Some((970, Some(reset_ms)))),
        json!("owner text"),
        "at most one note per threshold per execution"
    );

    // Скачок сразу за несколько порогов даёт одну пометку.
    let mut jump = InputQueue::new(launch("ask"));
    let command = Uuid::now_v7();
    jump.enqueue(&tx(command, 1, "x"));
    let content = written(jump.next(Some((955, None))))["message"]["content"].clone();
    assert_eq!(content.as_array().unwrap().len(), 2);
    assert_eq!(
        note(&content).as_deref(),
        Some("[canterlot] Weekly Claude limit: 95 % used.")
    );
}

// --- прерывание -------------------------------------------------------------------

/// Прогон A из `interrupt.jsonl`: ввод на `sleep 20`, кадры до SIGINT
/// (строки 5–38), после SIGINT строки 42–45 и выход с кодом 0.
async fn interrupted_run() -> (Fake, InputDriver, InterruptOutcome) {
    // Как в main: свой обработчик SIGINT снимает унаследованный SIG_IGN у потомка.
    let _interrupt =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();
    let mut fake = Fake::start(|dir| {
        let mut turn: Vec<String> = out_lines("interrupt", 5, 38)
            .into_iter()
            .map(|(_, frame)| frame)
            .collect();
        turn.push("@wait".into());
        write_frames(dir, "turn_1.out", &turn);
        let after: Vec<String> = out_lines("interrupt", 42, 45)
            .into_iter()
            .map(|(_, frame)| frame)
            .collect();
        write_frames(dir, "int.out", &after);
    })
    .await;
    let mut driver = InputDriver::new(InputQueue::new(launch("ask")), Parser::new());
    let nothing = tokio::time::timeout(WAIT, driver.interrupt(&mut fake.cli, DEADLINE))
        .await
        .unwrap();
    assert!(
        matches!(nothing, InterruptOutcome::NotApplicable),
        "no active turn, no signal"
    );
    let text = in_line("interrupt", 4)["message"]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    let outcome = tokio::time::timeout(
        WAIT,
        driver.transmit(
            &mut fake.cli,
            &tx(id(SLEEP_INPUT), 1, &text),
            None,
            DEADLINE,
        ),
    )
    .await
    .unwrap();
    assert!(matches!(outcome, TransmitOutcome::Accepted(_)));
    let mut seen = Vec::new();
    tokio::time::timeout(WAIT, async {
        while !seen.iter().any(|p| matches!(p, Parsed::RateLimit(_))) {
            driver.pump(&mut fake.cli).await;
            seen.extend(drain(&mut driver));
        }
    })
    .await
    .expect("the tool call never started");
    assert!(
        seen.iter().any(
            |p| matches!(p, Parsed::ToolCallComplete { tool_name, .. } if tool_name == "Bash")
        )
    );
    let interrupted = tokio::time::timeout(WAIT, driver.interrupt(&mut fake.cli, DEADLINE))
        .await
        .unwrap();
    (fake, driver, interrupted)
}

#[tokio::test]
async fn interrupt_sends_sigint_and_acknowledges_on_aborted_tools_result() {
    let (fake, mut driver, outcome) = interrupted_run().await;
    assert!(
        matches!(outcome, InterruptOutcome::Acknowledged),
        "got {outcome:?}"
    );
    let log = fake.stdin_log();
    assert_eq!(log.len(), 1, "SIGINT only: no control_request on stdin");
    assert_eq!(log[0]["uuid"], SLEEP_INPUT);
    let parsed = drain(&mut driver);
    assert!(parsed.iter().any(|p| matches!(
        p,
        Parsed::Event(AdapterEvent::RunFinished { input_command_id, .. }) if *input_command_id == id(SLEEP_INPUT)
    )));
    assert!(parsed.iter().any(|p| matches!(
        p,
        Parsed::TurnError { terminal_reason: Some(reason), .. } if reason == "aborted_tools"
    )));
    assert!(
        driver.queue.active().is_none(),
        "the interrupted turn is over"
    );
}

#[tokio::test]
async fn interrupt_exit_is_user_stop_not_a_crash() {
    let (mut fake, mut driver, outcome) = interrupted_run().await;
    assert!(
        matches!(outcome, InterruptOutcome::Acknowledged),
        "got {outcome:?}"
    );
    tokio::time::timeout(WAIT, async {
        while !driver.eof() {
            driver.pump(&mut fake.cli).await;
        }
    })
    .await
    .expect("the CLI did not close stdout after SIGINT");
    let status = tokio::time::timeout(WAIT, fake.cli.wait())
        .await
        .unwrap()
        .unwrap();
    let exited = exit_event(status);
    let AdapterEvent::ProcessExited { code, signal } = exited else {
        panic!("expected ProcessExited, got {exited:?}");
    };
    assert_eq!((code, signal), (Some(0), None));
    assert!(
        !drain(&mut driver).iter().any(|p| matches!(
            p,
            Parsed::Event(AdapterEvent::Diagnostic { required: true, .. })
        )),
        "the interrupted turn raises no required diagnostic"
    );
    // Тот же разбор выхода, что у супервизора для synthetic-exit-on-interrupt.
    use canterlot_worker::supervisor::exit_outcome;
    assert_eq!(exit_outcome(code, signal, true), ("stopped", "user_stop"));
    assert_eq!(
        exit_outcome(code, signal, false),
        ("stopped", "normal_exit")
    );
    assert_eq!(exit_outcome(None, Some(2), true), ("failed", "crashed"));
}

// --- чтение потока и управляющие ответы -------------------------------------------

#[tokio::test]
async fn stdout_reading_survives_cancelled_waits() {
    let dir = TempDir::new().unwrap();
    let script = dir.path().join("fake.sh");
    fake_exec::write_executable(
        &script,
        "#!/bin/sh\nprintf '{\"type\":\"x\",'\nsleep 0.5\nprintf '\"a\":1}\\n'\n",
        0o755,
    );
    let group = TreeContainment::new().unwrap();
    let spec = LaunchSpec {
        executable: &script,
        exec_dir: dir.path(),
        workdir: dir.path(),
        session: SessionArg::New(Uuid::nil()),
        model: "m",
        effort: "e",
        permission_mode: "owner",
        instructions: &owner_instructions(),
    };
    let mut cli = CliProcess::spawn(&spec, &ClaudeCodeConfig::default(), &group)
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), cli.next_line())
            .await
            .is_err(),
        "the line is not complete yet"
    );
    match tokio::time::timeout(WAIT, cli.next_line()).await.unwrap() {
        StdoutLine::Line(line) => assert_eq!(line, r#"{"type":"x","a":1}"#),
        other => panic!("expected the whole line, got {other:?}"),
    }
}

#[test]
fn control_response_success_is_read_from_the_recorded_frame() {
    let mut parser = Parser::new();
    let (_, recorded) = out_lines("interrupt", 90, 90).remove(0);
    let parsed = parser.feed(&recorded);
    assert!(matches!(
        parsed.as_slice(),
        [Parsed::ControlResponse { request_id, success: true }]
            if request_id == "probe-interrupt-b9039f8e-e82f-4e39-8cbc-d18b74389337"
    ));
    let error = json!({"type": "control_response", "response": {"subtype": "error", "request_id": "r1", "error": "no"}});
    let parsed = parser.feed(&error.to_string());
    assert!(matches!(
        parsed.as_slice(),
        [Parsed::ControlResponse { request_id, success: false }] if request_id == "r1"
    ));
}
