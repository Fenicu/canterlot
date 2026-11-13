//! Служба целиком: бинарник `serve` с ложным движком в дочернем процессе, протокол
//! по настоящему unix-сокету.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::net::UnixStream;

use canterlot_speech::config::file_sha256;
use canterlot_speech::frame::{self, Frame, KIND_AUDIO, KIND_JSON, ReadLimits};

const BIN: &str = env!("CARGO_BIN_EXE_canterlot-speech");

struct Service {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    log: PathBuf,
    child: Child,
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Service {
    /// `extra` — строки TOML верхнего уровня, `limits` — содержимое `[limits]`.
    fn launch(fake: &str, extra: &str, limits: &str) -> Service {
        let dir = tempfile::Builder::new()
            .prefix("cs")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::write(dir.path().join("m.gguf"), b"model").unwrap();
        std::fs::write(dir.path().join("v.gguf"), b"vad").unwrap();
        let sm = file_sha256(&dir.path().join("m.gguf")).unwrap();
        let sv = file_sha256(&dir.path().join("v.gguf")).unwrap();
        let socket = dir.path().join("s.sock");
        let config = dir.path().join("speech.toml");
        std::fs::write(
            &config,
            format!(
                "socket = \"{}\"\n{extra}\n[model]\npath = \"m.gguf\"\nsha256 = \"{sm}\"\n[vad]\npath = \"v.gguf\"\nsha256 = \"{sv}\"\n[limits]\n{limits}\n",
                socket.display()
            ),
        )
        .unwrap();
        let log = dir.path().join("log");
        let child = Command::new(BIN)
            .args(["serve", "--config"])
            .arg(&config)
            .args(["--fake-engine", fake])
            .env("RUST_LOG", "info")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let svc = Service {
            _dir: dir,
            socket,
            log,
            child,
        };
        let until = Instant::now() + Duration::from_secs(20);
        while !svc.socket.exists() {
            assert!(
                Instant::now() < until,
                "служба не открыла сокет: {}",
                svc.logs()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        svc
    }

    fn logs(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    async fn wait_log(&self, needle: &str, count: usize) {
        let until = Instant::now() + Duration::from_secs(20);
        while self.logs().matches(needle).count() < count {
            assert!(
                Instant::now() < until,
                "в логе нет {needle} x{count}:\n{}",
                self.logs()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn connect(&self) -> UnixStream {
        UnixStream::connect(&self.socket).await.unwrap()
    }

    async fn start(&self) -> UnixStream {
        let mut s = self.connect().await;
        send_json(
            &mut s,
            r#"{"type":"start","protocol":1,"sample_rate":16000,"format":"s16le","client":"test"}"#,
        )
        .await;
        s
    }

    async fn health(&self) -> Value {
        canterlot_speech::server::query_health(&self.socket)
            .await
            .unwrap()
    }
}

async fn send_json(s: &mut UnixStream, text: &str) {
    frame::write_frame(s, KIND_JSON, text.as_bytes())
        .await
        .unwrap();
}

/// 100 мс звука: речь — громкий сигнал, тишина — нули.
fn chunk(speech: bool) -> Vec<u8> {
    let v: i16 = if speech { 5000 } else { 0 };
    frame::pcm_to_bytes(&[v; 1600])
}

async fn send_audio(s: &mut UnixStream, pattern: &[(bool, u32)]) {
    for (speech, tenths) in pattern {
        for _ in 0..*tenths {
            // Служба могла уже закрыть соединение с ошибкой: её прочтёт read_all.
            if frame::write_frame(s, KIND_AUDIO, &chunk(*speech))
                .await
                .is_err()
            {
                return;
            }
        }
    }
}

/// Сообщения службы до закрытия соединения.
async fn read_all(s: &mut UnixStream) -> Vec<Value> {
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(
            Duration::from_secs(20),
            frame::read_frame(s, ReadLimits::SERVICE),
        )
        .await
        .expect("служба молчит")
        {
            Ok(Frame::Json(b)) => out.push(serde_json::from_slice(&b).unwrap()),
            Ok(Frame::Audio(_)) => panic!("звук от службы"),
            Err(_) => return out,
        }
    }
}

async fn read_one(s: &mut UnixStream) -> Value {
    match tokio::time::timeout(
        Duration::from_secs(20),
        frame::read_frame(s, ReadLimits::SERVICE),
    )
    .await
    .expect("служба молчит")
    .unwrap()
    {
        Frame::Json(b) => serde_json::from_slice(&b).unwrap(),
        Frame::Audio(_) => panic!("звук от службы"),
    }
}

fn types(msgs: &[Value]) -> Vec<String> {
    msgs.iter()
        .map(|m| match m["type"].as_str().unwrap() {
            "state" => format!("state:{}", m["state"].as_str().unwrap()),
            "error" => format!("error:{}", m["code"].as_str().unwrap()),
            t => t.to_owned(),
        })
        .collect()
}

fn pid_alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat.contains(") Z "),
        Err(_) => false,
    }
}

fn spawned_pids(log: &str) -> Vec<u32> {
    log.lines()
        .filter(|l| l.contains("engine_spawned"))
        .filter_map(|l| {
            l.split("pid=")
                .nth(1)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .collect()
}

const SPEECH_PAUSE_SPEECH: &[(bool, u32)] = &[(true, 10), (false, 8), (true, 10)];

#[tokio::test]
async fn dictation_buffers_while_loading_and_finishes_with_final() {
    let svc = Service::launch("load=500,decode=20", "", "");
    let h = svc.health().await;
    assert_eq!(h["ok"], true);
    assert_eq!(h["engine"], "cold");
    assert_eq!(h["mode"], Value::Null);
    assert_eq!(h["model"], "m");

    let mut s = svc.start().await;
    send_audio(&mut s, SPEECH_PAUSE_SPEECH).await;
    send_json(&mut s, r#"{"type":"finish"}"#).await;
    let msgs = read_all(&mut s).await;
    let t = types(&msgs);
    assert_eq!(t[0], "state:loading", "{t:?}");
    assert_eq!(t[1], "state:listening", "{t:?}");
    assert_eq!(msgs[1]["mode"], "gpu");
    assert_eq!(t.last().unwrap(), "final", "{t:?}");
    let fin = msgs.last().unwrap();
    assert_eq!(fin["reason"], "finish");
    let words: Vec<&str> = fin["text"].as_str().unwrap().split(' ').collect();
    assert_eq!(
        words.len(),
        2,
        "два сегмента, звук до загрузки не потерян: {fin}"
    );
    let seqs: Vec<u64> = msgs
        .iter()
        .filter(|m| m["type"] == "text")
        .map(|m| m["seq"].as_u64().unwrap())
        .collect();
    assert!(
        !seqs.is_empty() && seqs.windows(2).all(|w| w[0] < w[1]),
        "{seqs:?}"
    );

    let h = svc.health().await;
    assert_eq!(h["engine"], "warm");
    assert_eq!(h["mode"], "gpu");
    let logs = svc.logs();
    assert!(logs.contains("kind=\"dictation_finished\"") && logs.contains("outcome=\"finish\""));
    assert!(
        !logs.contains(fin["text"].as_str().unwrap()),
        "текст не попадает в лог"
    );
}

#[tokio::test]
async fn cancel_closes_without_reply_and_engine_stays_warm() {
    let svc = Service::launch("decode=20", "", "");
    let mut s = svc.start().await;
    send_audio(&mut s, &[(true, 10)]).await;
    send_json(&mut s, r#"{"type":"cancel"}"#).await;
    let t = types(&read_all(&mut s).await);
    assert!(
        t.iter().all(|m| m.starts_with("state") || m == "text"),
        "{t:?}"
    );
    svc.wait_log("outcome=\"cancel\"", 1).await;

    let mut s = svc.start().await;
    assert_eq!(
        read_one(&mut s).await["state"],
        "listening",
        "тёплый движок сразу слушает"
    );
    drop(s);
    svc.wait_log("kind=\"dictation_finished\"", 2).await;
    assert_eq!(spawned_pids(&svc.logs()).len(), 1);
}

#[tokio::test]
async fn frame_validation() {
    let svc = Service::launch("", "", "");
    for (bytes, code) in [
        (vec![1u8, 2, 3], "bad_audio"),
        (vec![], "bad_audio"),
        (vec![0u8; 8194], "bad_audio"),
    ] {
        let mut s = svc.start().await;
        frame::write_frame(&mut s, KIND_AUDIO, &bytes)
            .await
            .unwrap();
        let t = types(&read_all(&mut s).await);
        assert_eq!(t.last().unwrap(), &format!("error:{code}"), "{t:?}");
    }
    let mut s = svc.start().await;
    send_json(&mut s, r#"{"type":"finish"}"#).await;
    send_audio(&mut s, &[(true, 1)]).await;
    let t = types(&read_all(&mut s).await);
    assert!(
        t.last() == Some(&"error:bad_request".to_owned()) || t.last() == Some(&"final".to_owned()),
        "{t:?}"
    );

    let mut s = svc.connect().await;
    send_json(
        &mut s,
        r#"{"type":"start","protocol":1,"sample_rate":48000,"format":"s16le"}"#,
    )
    .await;
    assert_eq!(types(&read_all(&mut s).await), vec!["error:bad_audio"]);
    let mut s = svc.connect().await;
    send_json(
        &mut s,
        r#"{"type":"start","sample_rate":16000,"format":"s16le"}"#,
    )
    .await;
    assert_eq!(types(&read_all(&mut s).await), vec!["error:bad_request"]);
    let mut s = svc.connect().await;
    send_audio(&mut s, &[(true, 1)]).await;
    assert_eq!(types(&read_all(&mut s).await), vec!["error:bad_request"]);
}

#[tokio::test]
async fn audio_ahead_of_wall_clock_is_bad_audio() {
    let svc = Service::launch("", "", "max_lead = \"1s\"");
    let mut s = svc.start().await;
    send_audio(&mut s, &[(false, 30)]).await;
    let t = types(&read_all(&mut s).await);
    assert_eq!(t.last().unwrap(), "error:bad_audio", "{t:?}");
}

#[tokio::test]
async fn idle_and_lifetime_limits() {
    let svc = Service::launch(
        "",
        "",
        "idle = \"700ms\"\nmax_audio = \"1s\"\nlifetime = \"2s\"",
    );
    let mut s = svc.start().await;
    send_audio(&mut s, &[(false, 1)]).await;
    let started = Instant::now();
    let t = types(&read_all(&mut s).await);
    assert_eq!(t.last().unwrap(), "error:idle", "{t:?}");
    assert!(started.elapsed() < Duration::from_secs(2));

    let mut s = svc.start().await;
    let started = Instant::now();
    let mut msgs = Vec::new();
    for _ in 0..12 {
        // Тишина по 50 мс каждые 300 мс: idle не наступает, звук не кончается.
        frame::write_frame(&mut s, KIND_AUDIO, &frame::pcm_to_bytes(&[0; 800]))
            .await
            .unwrap_or_default();
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    msgs.extend(read_all(&mut s).await);
    let t = types(&msgs);
    assert_eq!(t.last().unwrap(), "error:lifetime", "{t:?}");
    assert!(started.elapsed() >= Duration::from_secs(2));
}

#[tokio::test]
async fn max_audio_finalizes_with_too_long() {
    let svc = Service::launch("decode=10", "", "max_audio = \"1s\"");
    let mut s = svc.start().await;
    send_audio(&mut s, &[(true, 15)]).await;
    let msgs = read_all(&mut s).await;
    let fin = msgs.last().unwrap();
    assert_eq!(fin["type"], "final", "{msgs:?}");
    assert_eq!(fin["reason"], "too_long");
    assert_eq!(fin["text"], "s1000", "ровно 1 с звука");
}

#[tokio::test]
async fn second_dictation_waits_third_is_busy() {
    let svc = Service::launch("", "", "queue_wait = \"1s\"");
    let mut first = svc.start().await;
    assert_eq!(read_one(&mut first).await["state"], "loading");
    assert_eq!(read_one(&mut first).await["state"], "listening");
    let mut second = svc.start().await;
    assert_eq!(read_one(&mut second).await["state"], "loading");
    let mut third = svc.start().await;
    assert_eq!(types(&read_all(&mut third).await), vec!["error:busy"]);
    assert_eq!(svc.health().await["busy"], true);
    let t = types(&read_all(&mut second).await);
    assert_eq!(
        t.last().unwrap(),
        "error:busy",
        "очередь не освободилась за 1 с"
    );

    let mut waiting = svc.start().await;
    assert_eq!(read_one(&mut waiting).await["state"], "loading");
    send_json(&mut first, r#"{"type":"finish"}"#).await;
    assert_eq!(read_all(&mut first).await.last().unwrap()["type"], "final");
    assert_eq!(read_one(&mut waiting).await["state"], "listening");
}

#[tokio::test]
async fn idle_unload_exits_engine_child() {
    let svc = Service::launch("", "idle_unload = \"1s\"", "");
    let mut s = svc.start().await;
    send_audio(&mut s, &[(true, 3)]).await;
    send_json(&mut s, r#"{"type":"finish"}"#).await;
    read_all(&mut s).await;
    let pid = spawned_pids(&svc.logs())[0];
    assert!(pid_alive(pid));
    assert_eq!(svc.health().await["engine"], "warm");
    svc.wait_log("kind=\"engine_unloaded\"", 1).await;
    assert!(svc.logs().contains("reason=\"idle\""));
    assert!(!pid_alive(pid), "дочерний процесс завершён, VRAM свободна");
    assert_eq!(svc.health().await["engine"], "cold");

    let mut s = svc.start().await;
    assert_eq!(
        read_one(&mut s).await["state"],
        "loading",
        "следующая диктовка грузит заново"
    );
    drop(s);
    svc.wait_log("kind=\"engine_spawned\"", 2).await;
}

#[tokio::test]
async fn stuck_native_run_is_killed_after_grace() {
    let svc = Service::launch("stall", "", "cancel_grace = \"500ms\"");
    let mut s = svc.start().await;
    send_audio(&mut s, &[(true, 10), (false, 8)]).await;
    // Окончательное задание висит в движке; отмена его не останавливает.
    tokio::time::sleep(Duration::from_millis(300)).await;
    send_json(&mut s, r#"{"type":"cancel"}"#).await;
    read_all(&mut s).await;
    svc.wait_log("reason=\"cancel_grace\"", 1).await;
    let pid = spawned_pids(&svc.logs())[0];
    assert!(!pid_alive(pid), "зависший движок убит");
    assert_eq!(svc.health().await["engine"], "cold");
}

#[tokio::test]
async fn load_failure_is_engine_unavailable_and_unhealthy() {
    let svc = Service::launch("fail_load", "", "");
    let mut s = svc.start().await;
    let t = types(&read_all(&mut s).await);
    assert_eq!(t.last().unwrap(), "error:engine_unavailable", "{t:?}");
    svc.wait_log("kind=\"engine_load_failed\"", 1).await;
    svc.wait_log("kind=\"dictation_finished\"", 1).await;
    assert_eq!(svc.health().await["ok"], false);
}

#[tokio::test]
async fn decode_failure_is_engine_failed() {
    let svc = Service::launch("fail_decode", "", "");
    let mut s = svc.start().await;
    send_audio(&mut s, SPEECH_PAUSE_SPEECH).await;
    let t = types(&read_all(&mut s).await);
    assert_eq!(t.last().unwrap(), "error:engine_failed", "{t:?}");
    svc.wait_log("reason=\"decode_failed\"", 1).await;
}

#[tokio::test]
async fn cpu_mode_sends_only_committed_text() {
    let svc = Service::launch("cpu", "", "");
    let mut s = svc.start().await;
    send_audio(&mut s, &[(true, 20), (false, 8), (true, 20)]).await;
    send_json(&mut s, r#"{"type":"finish"}"#).await;
    let msgs = read_all(&mut s).await;
    assert_eq!(msgs[1]["mode"], "cpu");
    for m in msgs.iter().filter(|m| m["type"] == "text") {
        assert_eq!(m["tentative"], "", "в режиме CPU нет хвоста: {m}");
    }
    assert_eq!(msgs.last().unwrap()["type"], "final");
}

#[tokio::test]
async fn health_command_exit_codes() {
    let svc = Service::launch("", "", "");
    let out = Command::new(BIN)
        .args(["health", "--socket"])
        .arg(&svc.socket)
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["engine"], "cold");

    let missing = Path::new("/tmp/canterlot-speech-no-such.sock");
    let out = Command::new(BIN)
        .args(["health", "--socket"])
        .arg(missing)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert!(String::from_utf8_lossy(&out.stderr).contains("speech_unavailable"));
}

#[tokio::test]
async fn idle_unload_is_a_clean_exit_without_running_atexit_handlers() {
    // Ложный движок регистрирует atexit с abort, как деструкторы ggml-cuda после
    // остановки рантайма CUDA: выход движка их запускать не должен.
    let svc = Service::launch("atexit_abort", "idle_unload = \"1s\"", "");
    let mut s = svc.start().await;
    send_audio(&mut s, &[(true, 3)]).await;
    send_json(&mut s, r#"{"type":"finish"}"#).await;
    assert_eq!(read_all(&mut s).await.last().unwrap()["type"], "final");
    svc.wait_log("reason=\"idle\"", 1).await;
    let logs = svc.logs();
    assert!(
        logs.contains("kind=\"engine_unloaded\"") && logs.contains("status=0"),
        "{logs}"
    );
    assert!(
        !logs.contains(" WARN ") && !logs.contains(" ERROR "),
        "чистая выгрузка без предупреждений:\n{logs}"
    );
}
