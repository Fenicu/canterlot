//! Живой алгоритм без ввода-вывода (docs/design/dictation.md §2): сегменты по VAD,
//! задания движку, окончательный текст и промежуточный хвост. Все границы —
//! абсолютные номера отсчётов 16 кГц от начала диктовки.

use std::collections::{BTreeMap, VecDeque};

use crate::config::LiveConfig;

pub const RATE: u64 = 16_000;

fn samples(d: std::time::Duration) -> u64 {
    (d.as_micros() as u64) * RATE / 1_000_000
}

#[derive(Debug, Clone, PartialEq)]
pub struct Params {
    pub tentative_interval: u64,
    pub pause: u64,
    pub max_segment: u64,
    pub forced_window: u64,
    pub preroll: u64,
    pub overlap: u64,
    pub threshold: f32,
    pub neg_threshold: f32,
}

impl Params {
    pub fn from_config(c: &LiveConfig) -> Params {
        Params {
            tentative_interval: samples(c.tentative_interval),
            pause: samples(c.pause),
            max_segment: samples(c.max_segment),
            forced_window: samples(c.forced_window),
            preroll: samples(c.preroll),
            overlap: samples(c.overlap),
            threshold: c.vad_threshold,
            neg_threshold: c.vad_neg_threshold,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Tentative,
    Commit,
    Final,
}

/// Задание движку: распознать `[start, end)`.
pub struct Job {
    pub id: u64,
    pub segment: u64,
    pub generation: u64,
    pub kind: JobKind,
    pub start: u64,
    pub end: u64,
    /// Слова, середина которых раньше этого смещения от `start`, отбрасываются (перекрытие).
    pub drop_ms: i64,
    pub pcm: Vec<i16>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("id", &self.id)
            .field("segment", &self.segment)
            .field("generation", &self.generation)
            .field("kind", &self.kind)
            .field("start", &self.start)
            .field("end", &self.end)
            .field("drop_ms", &self.drop_ms)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Default, PartialEq)]
pub struct Word {
    pub t0_ms: i64,
    pub t1_ms: i64,
    pub text: String,
}

/// Результат распознавания задания.
#[derive(Clone, Default, PartialEq)]
pub struct Decoded {
    pub text: String,
    pub words: Vec<Word>,
    pub decode_ms: u32,
}

impl std::fmt::Debug for Decoded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Decoded({} chars, {} words, {} ms)",
            self.text.chars().count(),
            self.words.len(),
            self.decode_ms
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateInfo {
    pub kind: JobKind,
    pub decode_ms: u32,
    /// Конец последнего слова результата, в отсчётах от начала диктовки.
    pub newest_word_end: Option<u64>,
}

#[derive(Clone, PartialEq)]
pub struct Update {
    pub committed: String,
    pub tentative: String,
    pub info: UpdateInfo,
}

impl std::fmt::Debug for Update {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Update(committed {} chars, tentative {} chars, {:?})",
            self.committed.chars().count(),
            self.tentative.chars().count(),
            self.info
        )
    }
}

#[derive(Debug)]
pub enum Out {
    Decode(Job),
    Text(Update),
    Final(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("распознавание не удалось: {0}")]
pub struct DecodeFailed(pub &'static str);

#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub segments: u32,
    pub forced_cuts: u32,
    pub tentatives: u32,
    pub commits: u32,
    pub stale: u32,
    pub tentative_ms: Vec<u32>,
    pub commit_ms: Vec<u32>,
}

#[derive(Debug, Clone, Copy)]
struct InFlight {
    id: u64,
    segment: u64,
    generation: u64,
    kind: JobKind,
    start: u64,
    drop_ms: i64,
}

pub struct Live {
    p: Params,
    frame: u64,
    tentative_enabled: bool,
    audio: Vec<i16>,
    base: u64,
    total: u64,
    probs: Vec<f32>,
    scan: usize,
    seg: u64,
    seg_start: u64,
    seg_forced: bool,
    speech: bool,
    silence_from: Option<u64>,
    next_tick: u64,
    next_job: u64,
    next_gen: u64,
    busy: Option<InFlight>,
    queue: VecDeque<Job>,
    tentative_due: bool,
    committed: String,
    open_tent: Option<(u64, String)>,
    held_tent: BTreeMap<u64, String>,
    last_emitted: (String, String),
    finishing: bool,
    final_sent: bool,
    stats: Stats,
}

impl std::fmt::Debug for Live {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Live")
            .field("total", &self.total)
            .field("segment", &self.seg)
            .field("seg_start", &self.seg_start)
            .field("speech", &self.speech)
            .finish_non_exhaustive()
    }
}

impl Live {
    /// `frame` — отсчётов на одну вероятность VAD (512 у Silero).
    pub fn new(p: Params, frame: u64) -> Live {
        assert!(frame > 0);
        Live {
            p,
            frame,
            tentative_enabled: true,
            audio: Vec::new(),
            base: 0,
            total: 0,
            probs: Vec::new(),
            scan: 0,
            seg: 0,
            seg_start: 0,
            seg_forced: false,
            speech: false,
            silence_from: None,
            next_tick: 0,
            next_job: 1,
            next_gen: 1,
            busy: None,
            queue: VecDeque::new(),
            tentative_due: false,
            committed: String::new(),
            open_tent: None,
            held_tent: BTreeMap::new(),
            last_emitted: (String::new(), String::new()),
            finishing: false,
            final_sent: false,
            stats: Stats::default(),
        }
    }

    /// Режим CPU: без промежуточного хвоста.
    pub fn set_tentative(&mut self, enabled: bool) {
        self.tentative_enabled = enabled;
        if !enabled {
            self.tentative_due = false;
        }
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn vad_frames(&self) -> usize {
        self.probs.len()
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    pub fn is_final_sent(&self) -> bool {
        self.final_sent
    }

    /// Отсчёты, которые надо отдать VAD, начиная с `from`.
    pub fn audio_from(&self, from: u64) -> &[i16] {
        let from = from.max(self.base);
        &self.audio[(from - self.base) as usize..]
    }

    /// Самый ранний отсчёт, который ещё хранится.
    pub fn retained_from(&self) -> u64 {
        self.base
    }

    pub fn push_audio(&mut self, pcm: &[i16]) -> Vec<Out> {
        if self.finishing {
            return Vec::new();
        }
        self.audio.extend_from_slice(pcm);
        self.total += pcm.len() as u64;
        self.check_tick();
        self.dispatch()
    }

    pub fn on_vad(&mut self, probs: &[f32]) -> Vec<Out> {
        self.probs.extend_from_slice(probs);
        self.scan_frames();
        self.check_tick();
        self.dispatch()
    }

    /// Звука больше не будет (VAD уже сброшен и разобран).
    pub fn finish(&mut self) -> Vec<Out> {
        if self.finishing {
            return Vec::new();
        }
        self.finishing = true;
        self.tentative_due = false;
        if let Some((_, t)) = self.open_tent.take() {
            self.held_tent.insert(self.seg, t);
        }
        let from = self.decode_from();
        if self.speech && self.total > from {
            let job = self.make_job(JobKind::Final, from, self.total);
            self.queue.push_back(job);
            self.stats.segments += 1;
        }
        self.seg += 1;
        self.speech = false;
        self.dispatch()
    }

    pub fn on_decoded(
        &mut self,
        id: u64,
        result: Result<Decoded, DecodeFailed>,
    ) -> Result<Vec<Out>, DecodeFailed> {
        let Some(flight) = self.busy else {
            return Ok(Vec::new());
        };
        if flight.id != id {
            return Ok(Vec::new());
        }
        self.busy = None;
        let decoded = result?;
        let (text, newest) = kept_text(&decoded, flight.drop_ms);
        let info = UpdateInfo {
            kind: flight.kind,
            decode_ms: decoded.decode_ms,
            newest_word_end: newest.map(|ms| flight.start + ms.max(0) as u64 * RATE / 1000),
        };
        let mut out = Vec::new();
        match flight.kind {
            JobKind::Commit | JobKind::Final => {
                self.stats.commits += 1;
                self.stats.commit_ms.push(decoded.decode_ms);
                append_text(&mut self.committed, &text);
                self.held_tent.remove(&flight.segment);
                out.extend(self.emit(info));
            }
            JobKind::Tentative => {
                self.stats.tentatives += 1;
                self.stats.tentative_ms.push(decoded.decode_ms);
                let fresh = !self.finishing
                    && flight.segment == self.seg
                    && self
                        .open_tent
                        .as_ref()
                        .is_none_or(|(g, _)| *g < flight.generation);
                if fresh {
                    self.open_tent = Some((flight.generation, text));
                    out.extend(self.emit(info));
                } else {
                    self.stats.stale += 1;
                }
            }
        }
        out.extend(self.dispatch());
        Ok(out)
    }

    fn emit(&mut self, info: UpdateInfo) -> Option<Out> {
        let mut tentative = String::new();
        for t in self.held_tent.values() {
            append_text(&mut tentative, t);
        }
        if let Some((_, t)) = &self.open_tent {
            append_text(&mut tentative, t);
        }
        if self.last_emitted.0 == self.committed && self.last_emitted.1 == tentative {
            return None;
        }
        self.last_emitted = (self.committed.clone(), tentative.clone());
        Some(Out::Text(Update {
            committed: self.committed.clone(),
            tentative,
            info,
        }))
    }

    fn decode_from(&self) -> u64 {
        if self.seg_forced {
            self.seg_start.saturating_sub(self.p.overlap).max(self.base)
        } else {
            self.seg_start
        }
    }

    fn make_job(&mut self, kind: JobKind, from: u64, to: u64) -> Job {
        let id = self.next_job;
        self.next_job += 1;
        let generation = self.next_gen;
        self.next_gen += 1;
        let lo = (from.max(self.base) - self.base) as usize;
        let hi = ((to - self.base) as usize).min(self.audio.len());
        Job {
            id,
            segment: self.seg,
            generation,
            kind,
            start: from,
            end: to,
            drop_ms: ((self.seg_start.saturating_sub(from)) * 1000 / RATE) as i64,
            pcm: self.audio[lo..hi].to_vec(),
        }
    }

    fn check_tick(&mut self) {
        if self.speech && self.tentative_enabled && !self.finishing && self.total >= self.next_tick
        {
            self.tentative_due = true;
            self.next_tick = self.total + self.p.tentative_interval;
        }
    }

    fn scan_frames(&mut self) {
        while self.scan < self.probs.len() {
            let i = self.scan;
            self.scan += 1;
            let fs = i as u64 * self.frame;
            let fe = fs + self.frame;
            if fe <= self.seg_start {
                continue;
            }
            let p = self.probs[i];
            if !self.speech {
                if p >= self.p.threshold {
                    self.speech = true;
                    self.seg_start = self.seg_start.max(fs.saturating_sub(self.p.preroll));
                    self.silence_from = None;
                    self.next_tick = fe + self.p.tentative_interval;
                } else {
                    let start = fe.saturating_sub(self.p.preroll);
                    if start > self.seg_start {
                        self.seg_start = start;
                        self.seg_forced = false;
                        self.trim();
                    }
                }
                continue;
            }
            if p >= self.p.threshold {
                self.silence_from = None;
            } else if p < self.p.neg_threshold {
                let s = *self.silence_from.get_or_insert(fs);
                if fe - s >= self.p.pause {
                    self.cut(s + self.p.pause / 2, false);
                    continue;
                }
            }
            if fe - self.seg_start >= self.p.max_segment {
                let c = self.forced_point(i);
                self.cut(c, true);
            }
        }
    }

    /// Середина кадра с наименьшей вероятностью речи в последних `forced_window`.
    fn forced_point(&self, last: usize) -> u64 {
        let end = (last as u64 + 1) * self.frame;
        let from = end
            .saturating_sub(self.p.forced_window)
            .max(self.seg_start + 1);
        let first = from.div_ceil(self.frame) as usize;
        let mut best = last;
        for i in first..=last {
            if self.probs[i] < self.probs[best] {
                best = i;
            }
        }
        best as u64 * self.frame + self.frame / 2
    }

    fn cut(&mut self, c: u64, forced: bool) {
        let c = c.min(self.total);
        let from = self.decode_from();
        let job = self.make_job(JobKind::Commit, from, c);
        self.queue.push_back(job);
        self.tentative_due = false;
        if let Some((_, t)) = self.open_tent.take() {
            self.held_tent.insert(self.seg, t);
        }
        self.stats.segments += 1;
        if forced {
            self.stats.forced_cuts += 1;
        }
        self.seg += 1;
        self.seg_start = c;
        self.seg_forced = forced;
        self.speech = false;
        self.silence_from = None;
        self.scan = (c / self.frame) as usize;
        self.trim();
    }

    fn trim(&mut self) {
        let keep = self.decode_from().min(self.total);
        if keep > self.base {
            self.audio.drain(..(keep - self.base) as usize);
            self.base = keep;
        }
    }

    fn dispatch(&mut self) -> Vec<Out> {
        if self.busy.is_some() || self.final_sent {
            return Vec::new();
        }
        if let Some(job) = self.queue.pop_front() {
            return vec![self.start(job)];
        }
        if self.finishing {
            self.final_sent = true;
            return vec![Out::Final(self.committed.clone())];
        }
        if self.tentative_due && self.speech && self.tentative_enabled {
            self.tentative_due = false;
            let from = self.decode_from();
            if self.total > from {
                let job = self.make_job(JobKind::Tentative, from, self.total);
                return vec![self.start(job)];
            }
        }
        Vec::new()
    }

    fn start(&mut self, job: Job) -> Out {
        self.busy = Some(InFlight {
            id: job.id,
            segment: job.segment,
            generation: job.generation,
            kind: job.kind,
            start: job.start,
            drop_ms: job.drop_ms,
        });
        Out::Decode(job)
    }
}

/// Текст результата без слов из перекрытия и конец последнего оставшегося слова (мс от начала задания).
fn kept_text(d: &Decoded, drop_ms: i64) -> (String, Option<i64>) {
    let newest_all = d.words.iter().map(|w| w.t1_ms).max();
    if drop_ms <= 0 || d.words.is_empty() {
        return (d.text.trim().to_owned(), newest_all);
    }
    let mut text = String::new();
    let mut newest = None;
    for w in d
        .words
        .iter()
        .filter(|w| (w.t0_ms + w.t1_ms) / 2 >= drop_ms)
    {
        append_text(&mut text, &w.text);
        newest = Some(newest.map_or(w.t1_ms, |n: i64| n.max(w.t1_ms)));
    }
    (text, newest)
}

fn append_text(to: &mut String, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if !to.is_empty() {
        to.push(' ');
    }
    to.push_str(text);
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: u64 = 512;
    const MS: u64 = RATE / 1000;

    fn params() -> Params {
        Params::from_config(&LiveConfig::default())
    }

    /// Ложный VAD: речь в заданных интервалах (мс), вероятность 0.9, иначе 0.05.
    struct Script {
        speech: Vec<(u64, u64)>,
        dips: Vec<(u64, f32)>,
    }

    impl Script {
        fn prob(&self, frame: usize) -> f32 {
            let mid_ms = (frame as u64 * FRAME + FRAME / 2) / MS;
            for (at, p) in &self.dips {
                if mid_ms / 32 == at / 32 {
                    return *p;
                }
            }
            if self.speech.iter().any(|(a, b)| mid_ms >= *a && mid_ms < *b) {
                0.9
            } else {
                0.05
            }
        }
    }

    /// Ложный движок: «настоящие» слова лежат на абсолютной сетке каждые 400 мс и длятся
    /// 200 мс; распознаётся слово, целиком попавшее в задание, текст — его абсолютное время.
    fn fake_decode(job: &Job) -> Decoded {
        let mut words = Vec::new();
        let (start, end) = (job.start / MS, job.end / MS);
        let mut abs = start.div_ceil(400) * 400;
        while abs + 200 <= end {
            let t0 = (abs - start) as i64;
            words.push(Word {
                t0_ms: t0,
                t1_ms: t0 + 200,
                text: format!("w{abs}"),
            });
            abs += 400;
        }
        Decoded {
            text: words
                .iter()
                .map(|w| w.text.clone())
                .collect::<Vec<_>>()
                .join(" "),
            words,
            decode_ms: 7,
        }
    }

    struct Harness {
        live: Live,
        script: Script,
        jobs: Vec<Job>,
        texts: Vec<Update>,
        finals: Vec<String>,
    }

    impl Harness {
        fn new(script: Script) -> Harness {
            Harness {
                live: Live::new(params(), FRAME),
                script,
                jobs: Vec::new(),
                texts: Vec::new(),
                finals: Vec::new(),
            }
        }

        fn take(&mut self, out: Vec<Out>) {
            for o in out {
                match o {
                    Out::Decode(j) => self.jobs.push(j),
                    Out::Text(u) => self.texts.push(u),
                    Out::Final(t) => self.finals.push(t),
                }
            }
        }

        /// Подаёт звук кусками по 100 мс до `ms` и оценки VAD по всем целым кадрам.
        fn feed_to(&mut self, ms: u64) {
            while self.live.total() < ms * MS {
                let out = self.live.push_audio(&[0; 1600]);
                self.take(out);
                let frames = (self.live.total() / FRAME) as usize;
                let have = self.live.vad_frames();
                let probs: Vec<f32> = (have..frames).map(|i| self.script.prob(i)).collect();
                let out = self.live.on_vad(&probs);
                self.take(out);
            }
        }

        /// Завершает самое старое задание ложным результатом.
        fn complete_one(&mut self) -> Job {
            let job = self.jobs.remove(0);
            let d = fake_decode(&job);
            let out = self.live.on_decoded(job.id, Ok(d)).unwrap();
            self.take(out);
            job
        }

        fn drain(&mut self) {
            while !self.jobs.is_empty() {
                self.complete_one();
            }
        }

        fn kinds(&self) -> Vec<JobKind> {
            self.jobs.iter().map(|j| j.kind).collect()
        }
    }

    #[test]
    fn cut_at_pause_commits_segment_and_keeps_the_rest() {
        let mut h = Harness::new(Script {
            speech: vec![(1000, 3000), (4000, 5000)],
            dips: vec![],
        });
        h.live.set_tentative(false);
        h.feed_to(3400);
        assert!(h.jobs.is_empty(), "пауза короче 500 мс не режет");
        h.feed_to(3700);
        assert_eq!(h.kinds(), vec![JobKind::Commit]);
        let first = h.complete_one();
        // Начало речи по границе кадра VAD (32 мс) минус 200 мс.
        assert!(
            first.start >= 760 * MS && first.start <= 800 * MS,
            "{first:?}"
        );
        assert!(first.end > 3000 * MS && first.end < 3500 * MS, "{first:?}");
        h.feed_to(6000);
        let second = h.complete_one();
        assert_eq!(second.kind, JobKind::Commit);
        assert!(second.start >= first.end, "сегменты не пересекаются");
        assert!(second.start >= 3760 * MS && second.start <= 3800 * MS);
        assert_eq!(second.drop_ms, 0);
        let out = h.live.finish();
        h.take(out);
        assert_eq!(h.finals.len(), 1);
        let words: Vec<&str> = h.finals[0].split(' ').collect();
        assert!(
            words.contains(&"w800") && words.contains(&"w4000"),
            "{words:?}"
        );
    }

    #[test]
    fn forced_cut_at_max_segment_overlaps_and_drops_repeated_words() {
        let mut h = Harness::new(Script {
            speech: vec![(0, 30_000)],
            dips: vec![(17_000, 0.4)],
        });
        h.live.set_tentative(false);
        h.feed_to(19_900);
        assert!(h.jobs.is_empty());
        h.feed_to(20_100);
        assert_eq!(h.kinds(), vec![JobKind::Commit]);
        let cut = h.jobs[0].end;
        assert!(
            cut / MS >= 16_990 && cut / MS <= 17_040,
            "разрез у самого тихого кадра, а не на 20 с: {}",
            cut / MS
        );
        h.complete_one();
        h.feed_to(30_000);
        let out = h.live.finish();
        h.take(out);
        let last = h.jobs.last().unwrap();
        assert_eq!(last.kind, JobKind::Final);
        assert_eq!(last.start, cut - params().overlap, "перекрытие 300 мс");
        assert_eq!(last.drop_ms, 300);
        h.drain();
        let text = &h.finals[0];
        let words: Vec<&str> = text.split(' ').collect();
        let mut unique = words.clone();
        unique.dedup();
        assert_eq!(words, unique, "слова на стыке не повторяются: {text}");
        assert!(text.contains("w16800") && text.contains("w17200"), "{text}");
        assert_eq!(h.live.stats().forced_cuts, 1);
    }

    #[test]
    fn tentative_ticks_are_skipped_while_busy_never_queued() {
        let mut h = Harness::new(Script {
            speech: vec![(0, 10_000)],
            dips: vec![],
        });
        h.feed_to(700);
        assert_eq!(h.kinds(), vec![JobKind::Tentative]);
        h.feed_to(2700);
        assert_eq!(h.jobs.len(), 1, "пока движок занят, хвосты не копятся");
        h.complete_one();
        assert_eq!(h.kinds(), vec![JobKind::Tentative]);
        assert_eq!(h.jobs[0].end, 2700 * MS, "хвост по свежему звуку");
        assert!(h.texts.last().unwrap().tentative.contains("w0"));
        h.complete_one();
        assert!(h.jobs.is_empty());
    }

    #[test]
    fn commit_has_priority_and_stale_tentative_is_rejected() {
        let mut h = Harness::new(Script {
            speech: vec![(0, 2000), (3000, 5000)],
            dips: vec![],
        });
        h.feed_to(1500);
        h.complete_one();
        let shown = h.texts.last().unwrap().tentative.clone();
        assert!(!shown.is_empty());
        h.feed_to(1900);
        assert_eq!(h.kinds(), vec![JobKind::Tentative]);
        // Разрез при незавершённом хвосте сегмента 0.
        h.feed_to(2700);
        assert_eq!(h.jobs.len(), 1);
        let texts_before = h.texts.len();
        let stale = h.complete_one();
        assert_eq!(stale.kind, JobKind::Tentative);
        assert_eq!(
            h.live.stats().stale,
            1,
            "хвост зафиксированного сегмента отброшен"
        );
        assert_eq!(h.texts.len(), texts_before);
        assert_eq!(
            h.kinds(),
            vec![JobKind::Commit],
            "окончательное задание первым"
        );
        h.feed_to(3800);
        assert_eq!(h.jobs.len(), 1, "пока идёт окончательное, хвост ждёт");
        // Пока окончательное не вернулось, старый хвост остаётся на экране.
        let commit = h.complete_one();
        assert_eq!(commit.kind, JobKind::Commit);
        let after_commit = &h.texts[texts_before];
        assert!(after_commit.committed.contains("w0"));
        assert_eq!(after_commit.info.kind, JobKind::Commit);
        assert_eq!(h.kinds(), vec![JobKind::Tentative]);
        assert!(h.jobs[0].start >= commit.end);
        assert!(shown.starts_with("w0"));
    }

    #[test]
    fn held_tentative_is_shown_until_its_commit_arrives() {
        let mut h = Harness::new(Script {
            speech: vec![(0, 2000), (2600, 5000)],
            dips: vec![],
        });
        h.feed_to(1500);
        h.complete_one();
        h.feed_to(2700);
        while h.kinds().first() == Some(&JobKind::Tentative) {
            h.complete_one();
        }
        assert_eq!(h.kinds(), vec![JobKind::Commit]);
        h.feed_to(3300);
        let last = h.texts.last().unwrap();
        assert!(last.committed.is_empty());
        assert!(last.tentative.contains("w0"));
        h.complete_one();
        let last = h.texts.last().unwrap();
        assert!(last.committed.contains("w0"));
    }

    #[test]
    fn finish_flushes_open_segment_to_the_last_sample() {
        let mut h = Harness::new(Script {
            speech: vec![(500, 4000)],
            dips: vec![],
        });
        h.live.set_tentative(false);
        h.feed_to(4000);
        assert!(h.jobs.is_empty());
        let out = h.live.finish();
        h.take(out);
        assert_eq!(h.kinds(), vec![JobKind::Final]);
        assert_eq!(h.jobs[0].end, 4000 * MS);
        assert_eq!(h.jobs[0].pcm.len() as u64, h.jobs[0].end - h.jobs[0].start);
        assert!(h.finals.is_empty());
        h.drain();
        assert_eq!(h.finals.len(), 1);
        assert!(h.finals[0].ends_with("w3600"), "{}", h.finals[0]);
        assert!(
            h.live.push_audio(&[0; 1600]).is_empty(),
            "после finish звук не принимается"
        );
    }

    #[test]
    fn finish_without_speech_is_empty_final() {
        let mut h = Harness::new(Script {
            speech: vec![],
            dips: vec![],
        });
        h.feed_to(3000);
        assert!(h.live.retained_from() > 2000 * MS, "тишина не копится");
        let out = h.live.finish();
        h.take(out);
        assert!(h.jobs.is_empty());
        assert_eq!(h.finals, vec![String::new()]);
    }

    #[test]
    fn finish_waits_for_queued_commits_in_order() {
        let mut h = Harness::new(Script {
            speech: vec![(0, 1000), (1700, 2500)],
            dips: vec![],
        });
        h.live.set_tentative(false);
        h.feed_to(2500);
        let out = h.live.finish();
        h.take(out);
        assert_eq!(h.kinds(), vec![JobKind::Commit]);
        h.complete_one();
        assert_eq!(h.kinds(), vec![JobKind::Final]);
        h.complete_one();
        assert_eq!(h.finals.len(), 1);
        assert!(h.finals[0].starts_with("w0"));
        assert!(h.finals[0].contains("w1600"));
    }

    #[test]
    fn cpu_mode_has_no_tentative_decodes() {
        let mut h = Harness::new(Script {
            speech: vec![(0, 3000), (4000, 9000)],
            dips: vec![],
        });
        h.live.set_tentative(false);
        h.feed_to(10_000);
        h.drain();
        assert_eq!(h.live.stats().tentatives, 0);
        assert_eq!(h.live.stats().commits, 2);
        assert!(h.texts.iter().all(|t| t.tentative.is_empty()));
    }

    #[test]
    fn decode_failure_is_an_error() {
        let mut h = Harness::new(Script {
            speech: vec![(0, 3000)],
            dips: vec![],
        });
        h.feed_to(800);
        let job = h.jobs.remove(0);
        assert_eq!(
            h.live
                .on_decoded(job.id, Err(DecodeFailed("failed")))
                .unwrap_err(),
            DecodeFailed("failed")
        );
    }

    #[test]
    fn debug_output_has_no_text() {
        let d = Decoded {
            text: "секрет".into(),
            words: vec![],
            decode_ms: 1,
        };
        assert!(!format!("{d:?}").contains("секрет"));
        let u = Update {
            committed: "секрет".into(),
            tentative: "тайна".into(),
            info: UpdateInfo {
                kind: JobKind::Commit,
                decode_ms: 1,
                newest_word_end: None,
            },
        };
        let s = format!("{u:?}");
        assert!(!s.contains("секрет") && !s.contains("тайна"));
    }
}
