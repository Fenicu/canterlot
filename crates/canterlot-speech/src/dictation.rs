//! Одна диктовка: звук от клиента, события движка, живой алгоритм, пределы и отмена.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;

use crate::config::Limits;
use crate::engine::{EngineLink, Event, Loaded, Mode, Request};
use crate::live::{DecodeFailed, Live, Out, Params, RATE, Update};

/// Сообщение клиента после `start`; закрытый канал равен `Cancel`.
pub enum ClientMsg {
    Audio(Vec<i16>),
    Finish,
    Cancel,
    /// Нарушение протокола, найденное читателем сокета.
    Error(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalReason {
    Finish,
    TooLong,
}

impl FinalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FinalReason::Finish => "finish",
            FinalReason::TooLong => "too_long",
        }
    }
}

/// Куда идут сообщения службы.
pub trait Sink: Send {
    fn listening(&mut self, mode: Mode);
    fn loading(&mut self);
    fn text(&mut self, update: Update);
    fn final_text(&mut self, text: String, reason: FinalReason);
    fn error(&mut self, code: &'static str);
}

/// Что делать с движком после диктовки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineFate {
    Keep,
    /// Убить дочерний процесс: следующая диктовка загрузит заново.
    Kill(&'static str),
}

#[derive(Debug)]
pub struct Outcome {
    /// `finish`, `too_long`, `cancel` или код ошибки.
    pub result: &'static str,
    pub fate: EngineFate,
    pub loaded: Option<Loaded>,
    pub audio_samples: u64,
    pub frames: u64,
    pub stats: crate::live::Stats,
    pub listening_after: Option<Duration>,
    pub final_after_finish: Option<Duration>,
}

pub struct Dictation<'a, S: Sink> {
    pub link: &'a mut EngineLink,
    pub loaded: Option<Loaded>,
    pub params: Params,
    pub limits: Limits,
    pub started: Instant,
    /// Будится, если писатель не смог отдать кадр клиенту.
    pub writer_failed: Option<tokio::sync::watch::Receiver<bool>>,
    /// Остановка службы.
    pub stop: Option<tokio::sync::watch::Receiver<bool>>,
    pub sink: S,
}

const VAD_CHUNK: u64 = 2 * RATE;

struct State {
    live: Option<Live>,
    pending: Vec<i16>,
    received: u64,
    frames: u64,
    vad_sent: u64,
    vad_inflight: bool,
    flush: Flush,
    decode: Option<u64>,
    finish_at: Option<Instant>,
    reason: FinalReason,
    last_audio: Instant,
    listening_at: Option<Instant>,
}

#[derive(PartialEq, Eq)]
enum Flush {
    No,
    Sent,
    Done,
}

enum Step {
    Continue,
    Done(&'static str, EngineFate),
}

impl<S: Sink> Dictation<'_, S> {
    pub async fn run(mut self, mut client: mpsc::Receiver<ClientMsg>) -> (Outcome, S) {
        let mut st = State {
            live: None,
            pending: Vec::new(),
            received: 0,
            frames: 0,
            vad_sent: 0,
            vad_inflight: false,
            flush: Flush::No,
            decode: None,
            finish_at: None,
            reason: FinalReason::Finish,
            last_audio: Instant::now(),
            listening_at: None,
        };
        if let Some(l) = self.loaded.clone() {
            self.on_loaded(&mut st, l);
        } else {
            self.sink.loading();
        }
        let lifetime_end = self.started + self.limits.lifetime;
        let start_end = self.started + self.limits.start_deadline;
        let mut writer_failed = self.writer_failed.take();
        let mut stop = self.stop.take();
        let (result, fate) = loop {
            let mut deadline = lifetime_end;
            if st.live.is_none() {
                deadline = deadline.min(start_end);
            }
            match st.finish_at {
                None => deadline = deadline.min(st.last_audio + self.limits.idle),
                Some(f) => deadline = deadline.min(f + self.limits.finish_timeout),
            }
            let step = tokio::select! {
                msg = client.recv(), if st.finish_at.is_none() || st.reason == FinalReason::TooLong => {
                    self.on_client(&mut st, msg)
                }
                ev = self.link.rx.recv() => self.on_event(&mut st, ev),
                _ = wait_failed(&mut writer_failed) => Step::Done("write_timeout", EngineFate::Keep),
                _ = wait_failed(&mut stop) => Step::Done("shutting_down", EngineFate::Keep),
                _ = tokio::time::sleep_until(deadline) => {
                    let now = Instant::now();
                    if now >= lifetime_end {
                        Step::Done("lifetime", EngineFate::Keep)
                    } else if st.live.is_none() && now >= start_end {
                        Step::Done("start_timeout", EngineFate::Kill("start_timeout"))
                    } else if st.finish_at.is_some() {
                        Step::Done("engine_failed", EngineFate::Kill("finish_timeout"))
                    } else {
                        Step::Done("idle", EngineFate::Keep)
                    }
                }
            };
            match step {
                Step::Continue => {}
                Step::Done(result, fate) => break (result, fate),
            }
            if let Some(step) = self.pump(&mut st) {
                match step {
                    Step::Continue => {}
                    Step::Done(result, fate) => break (result, fate),
                }
            }
        };
        let fate = match result {
            "finish" | "too_long" => fate,
            other => {
                if other != "cancel" && other != "write_timeout" {
                    self.sink.error(other);
                }
                self.abort(&mut st, fate).await
            }
        };
        let outcome = Outcome {
            result,
            fate,
            loaded: self.loaded.clone(),
            audio_samples: st.received,
            frames: st.frames,
            stats: st
                .live
                .as_ref()
                .map(|l| l.stats().clone())
                .unwrap_or_default(),
            listening_after: st.listening_at.map(|t| t - self.started),
            final_after_finish: None,
        };
        let mut outcome = outcome;
        if matches!(result, "finish" | "too_long") {
            outcome.final_after_finish = st.finish_at.map(|f| f.elapsed());
        }
        (outcome, self.sink)
    }

    fn on_loaded(&mut self, st: &mut State, l: Loaded) {
        let mut live = Live::new(self.params.clone(), l.vad_frame.max(1) as u64);
        live.set_tentative(l.mode == Mode::Gpu);
        let _ = self.link.tx.send(Request::VadReset);
        let pending = std::mem::take(&mut st.pending);
        let outs = live.push_audio(&pending);
        st.live = Some(live);
        st.listening_at = Some(Instant::now());
        self.sink.listening(l.mode);
        self.loaded = Some(l);
        let _ = self.handle_outs(st, outs);
    }

    fn on_client(&mut self, st: &mut State, msg: Option<ClientMsg>) -> Step {
        match msg {
            None | Some(ClientMsg::Cancel) => Step::Done("cancel", EngineFate::Keep),
            Some(ClientMsg::Error(code)) => Step::Done(code, EngineFate::Keep),
            Some(ClientMsg::Finish) => {
                if st.finish_at.is_none() {
                    st.finish_at = Some(Instant::now());
                }
                Step::Continue
            }
            Some(ClientMsg::Audio(mut pcm)) => {
                if st.finish_at.is_some() {
                    // Звук после too_long отбрасывается молча.
                    return Step::Continue;
                }
                st.frames += 1;
                st.last_audio = Instant::now();
                let max = self.limits.max_audio.as_millis() as u64 * RATE / 1000;
                let room = max.saturating_sub(st.received) as usize;
                if pcm.len() >= room {
                    pcm.truncate(room);
                    st.reason = FinalReason::TooLong;
                    st.finish_at = Some(Instant::now());
                }
                st.received += pcm.len() as u64;
                let audio_ms = st.received * 1000 / RATE;
                let wall_ms = self.started.elapsed().as_millis() as u64;
                if audio_ms > wall_ms + self.limits.max_lead.as_millis() as u64 {
                    return Step::Done("bad_audio", EngineFate::Keep);
                }
                match &mut st.live {
                    Some(live) => {
                        let outs = live.push_audio(&pcm);
                        return self.handle_outs(st, outs);
                    }
                    None => st.pending.extend_from_slice(&pcm),
                }
                Step::Continue
            }
        }
    }

    fn on_event(&mut self, st: &mut State, ev: Option<Event>) -> Step {
        let Some(ev) = ev else {
            return Step::Done("engine_failed", EngineFate::Kill("engine_exited"));
        };
        match ev {
            Event::Loaded(l) => {
                self.on_loaded(st, l);
                Step::Continue
            }
            Event::LoadFailed(code) => {
                tracing::warn!(kind = "engine_load_failed", code = %code, "engine failed to load");
                Step::Done("engine_unavailable", EngineFate::Kill("load_failed"))
            }
            Event::Exited => Step::Done("engine_failed", EngineFate::Kill("engine_exited")),
            Event::Vad(Err(_)) => Step::Done("engine_failed", EngineFate::Kill("vad_failed")),
            Event::Vad(Ok(probs)) => {
                st.vad_inflight = false;
                let Some(live) = &mut st.live else {
                    return Step::Continue;
                };
                let mut outs = live.on_vad(&probs);
                if st.flush == Flush::Sent {
                    st.flush = Flush::Done;
                    outs.extend(live.finish());
                }
                self.handle_outs(st, outs)
            }
            Event::Decoded { id, result } => {
                if st.decode != Some(id) {
                    return Step::Continue;
                }
                st.decode = None;
                let Some(live) = &mut st.live else {
                    return Step::Continue;
                };
                let result = result.map_err(|_| DecodeFailed("failed"));
                match live.on_decoded(id, result) {
                    Ok(outs) => self.handle_outs(st, outs),
                    Err(_) => Step::Done("engine_failed", EngineFate::Kill("decode_failed")),
                }
            }
        }
    }

    fn handle_outs(&mut self, st: &mut State, outs: Vec<Out>) -> Step {
        for out in outs {
            match out {
                Out::Decode(job) => {
                    st.decode = Some(job.id);
                    let _ = self.link.tx.send(Request::Decode {
                        id: job.id,
                        pcm: job.pcm,
                    });
                }
                Out::Text(u) => self.sink.text(u),
                Out::Final(text) => {
                    self.sink.final_text(text, st.reason);
                    return Step::Done(st.reason.as_str(), EngineFate::Keep);
                }
            }
        }
        Step::Continue
    }

    /// Подаёт звук в VAD по одному заданию; после `finish` — сброс VAD.
    fn pump(&mut self, st: &mut State) -> Option<Step> {
        let live = st.live.as_ref()?;
        if st.vad_inflight {
            return None;
        }
        let total = live.total();
        if st.vad_sent < total {
            let end = total.min(st.vad_sent + VAD_CHUNK);
            let chunk = live.audio_from(st.vad_sent)[..(end - st.vad_sent) as usize].to_vec();
            st.vad_sent = end;
            st.vad_inflight = true;
            let _ = self.link.tx.send(Request::VadFeed(chunk));
        } else if st.finish_at.is_some() && st.flush == Flush::No {
            st.flush = Flush::Sent;
            st.vad_inflight = true;
            let _ = self.link.tx.send(Request::VadFlush);
        }
        None
    }

    /// Отмена: ждать возврата задания VAD и распознавания до `cancel_grace`, иначе убить движок.
    async fn abort(&mut self, st: &mut State, fate: EngineFate) -> EngineFate {
        if fate != EngineFate::Keep {
            return fate;
        }
        if let Some(id) = st.decode {
            let _ = self.link.tx.send(Request::Cancel { id });
        }
        let grace = tokio::time::sleep(self.limits.cancel_grace);
        tokio::pin!(grace);
        while st.decode.is_some() || st.vad_inflight || (st.live.is_none() && self.loaded.is_none())
        {
            tokio::select! {
                _ = &mut grace => return EngineFate::Kill("cancel_grace"),
                ev = self.link.rx.recv() => match ev {
                    None | Some(Event::Exited) => return EngineFate::Kill("engine_exited"),
                    Some(Event::Decoded { id, .. }) if st.decode == Some(id) => st.decode = None,
                    Some(Event::Vad(_)) => st.vad_inflight = false,
                    Some(Event::Loaded(l)) => self.loaded = Some(l),
                    Some(Event::LoadFailed(_)) => return EngineFate::Kill("load_failed"),
                    Some(_) => {}
                }
            }
        }
        EngineFate::Keep
    }
}

async fn wait_failed(rx: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    match rx {
        Some(rx) => {
            if rx.wait_for(|failed| *failed).await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        None => std::future::pending().await,
    }
}

/// Процентиль по возрастанию (p в процентах).
pub fn percentile(values: &[u32], p: u32) -> u32 {
    if values.is_empty() {
        return 0;
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let idx = ((v.len() - 1) as u64 * p as u64).div_ceil(100) as usize;
    v[idx.min(v.len() - 1)]
}
