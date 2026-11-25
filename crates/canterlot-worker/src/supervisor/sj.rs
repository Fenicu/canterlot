use std::{
    collections::{BTreeMap, VecDeque},
    path::Path,
};

use canterlot_journal::{Journal, JournalError, JournalId, JournalOptions};
use canterlot_proto::{
    limits::{MAX_EVENT_BATCH_BYTES, MAX_SJ_BATCH_RECORDS, check_record_batch},
    worker::{
        ipc::SjBatch,
        v1::{JournalRecord, JournalRecords, RecordBody, RequestOpened, record_body::Body},
    },
};
use prost::Message;
use uuid::Uuid;

use super::SupervisorError;

const WINDOW: usize = 4;

/// Сколько байт очереди записи сопровождение копит, пока журнал не пишется, прежде чем
/// перестать принимать новые команды. Обязательные итоги остановки (закрытие блоков,
/// исходы вводов, итог исполнения, `journal_closed`) пишутся и сверх этого: их число
/// ограничено, потому что новых вводов в остановке нет.
const BACKLOG_LIMIT: usize = 8 * 1024 * 1024;

pub(super) struct RequestState {
    pub opened: RequestOpened,
    pub opened_seq: u64,
    pub transmission: Option<String>,
    pub terminal: bool,
}

struct Ship {
    next: u64,
    inflight: VecDeque<u64>,
}

pub(super) struct Sj {
    journal: Journal,
    journal_id: Uuid,
    ship: Option<Ship>,
    pending: Vec<Vec<u8>>,
    pending_bytes: usize,
    backlog_limit: usize,
    requests: BTreeMap<String, RequestState>,
    closed_reason: Option<String>,
    /// Наибольшее подтверждение хаба, которое ещё не легло в файл ack: запись ack могла
    /// отказать, а хаб второй раз его не пришлёт.
    owed_ack: u64,
    /// Номер последней записи, которая свидетельствует о прогрессе хода.
    progress_seq: u64,
    #[cfg(test)]
    commits: usize,
}

impl Sj {
    pub fn create(exec_dir: &Path) -> Result<Sj, SupervisorError> {
        let journal = if exec_dir.join("sj").try_exists()? {
            Journal::open(&exec_dir.join("sj"), JournalOptions::default())?.0
        } else {
            Journal::create(
                &exec_dir.join("sj"),
                JournalId(Uuid::now_v7()),
                JournalOptions::default(),
            )?
        };
        let journal_id = journal.id().0;
        let mut sj = Sj {
            journal,
            journal_id,
            ship: None,
            pending: Vec::new(),
            pending_bytes: 0,
            backlog_limit: BACKLOG_LIMIT,
            requests: BTreeMap::new(),
            closed_reason: None,
            owed_ack: 0,
            progress_seq: 0,
            #[cfg(test)]
            commits: 0,
        };
        let mut from = sj.journal.first_seq();
        while from <= sj.last_seq() {
            for record in sj.journal.read_from(from, MAX_EVENT_BATCH_BYTES)? {
                from = record.seq + 1;
                if let Some(body) = RecordBody::decode(record.payload.as_slice())?.body {
                    sj.observe(record.seq, &body);
                }
            }
        }
        Ok(sj)
    }

    fn observe(&mut self, seq: u64, body: &Body) {
        if canterlot_proto::is_stream_progress(body) {
            self.progress_seq = seq;
        }
        match body {
            Body::JournalClosed(f) => self.closed_reason = Some(f.reason.clone()),
            Body::RequestOpened(opened) => {
                self.requests
                    .entry(opened.request_id.clone())
                    .or_insert_with(|| RequestState {
                        opened: opened.clone(),
                        opened_seq: seq,
                        transmission: None,
                        terminal: false,
                    });
            }
            Body::DecisionTransmissionStarted(f) => {
                if let Some(request) = self.requests.get_mut(&f.request_id) {
                    request.transmission = Some(f.decision_id.clone());
                }
            }
            Body::DecisionApplied(f) => {
                if let Some(request) = self.requests.get_mut(&f.request_id)
                    && request.transmission.as_deref() == Some(&f.decision_id)
                {
                    request.terminal = true;
                    // Терминальный текст в памяти больше не нужен: решение уже у хаба.
                    request.opened.payload_json = Vec::new();
                }
            }
            Body::DecisionNotApplied(f) => {
                if let Some(request) = self.requests.get_mut(&f.request_id)
                    && (f.reason == "request_closed"
                        || request.transmission.as_deref() == Some(&f.decision_id))
                    && !matches!(
                        f.reason.as_str(),
                        "already_transmitted" | "stale_generation" | "invalid_decision"
                    )
                {
                    request.terminal = true;
                    request.opened.payload_json = Vec::new();
                }
            }
            Body::RequestClosed(f) => {
                if let Some(request) = self.requests.get_mut(&f.request_id) {
                    request.terminal = true;
                    request.opened.payload_json = Vec::new();
                }
            }
            _ => {}
        }
    }

    pub fn request(&self, id: &str) -> Option<&RequestState> {
        self.requests.get(id)
    }

    pub fn closed_reason(&self) -> Option<String> {
        self.closed_reason.clone()
    }

    pub fn open_requests(&self) -> Vec<RequestOpened> {
        if self.closed_reason.is_some() {
            return Vec::new();
        }
        self.requests
            .values()
            .filter(|r| !r.terminal)
            .map(|r| r.opened.clone())
            .collect()
    }

    pub fn has_open_requests(&self) -> bool {
        self.closed_reason.is_none() && self.requests.values().any(|r| !r.terminal)
    }

    pub fn journal_id(&self) -> Uuid {
        self.journal_id
    }

    pub fn progress_seq(&self) -> u64 {
        self.progress_seq
    }

    pub fn last_seq(&self) -> u64 {
        self.journal.last_seq()
    }

    #[cfg(test)]
    pub fn first_seq(&self) -> u64 {
        self.journal.first_seq()
    }

    pub fn acked_seq(&self) -> u64 {
        self.journal.acked_seq()
    }

    /// Номер следующей записи: журнал переживает перезапуск сопровождения, и ревизия вместе с ним.
    pub fn next_seq(&self) -> u64 {
        self.journal.last_seq() + self.pending.len() as u64 + 1
    }

    pub fn append(&mut self, body: Body) -> Result<u64, SupervisorError> {
        if self.closed_reason.is_some() {
            return Err(SupervisorError::Spec);
        }
        let seq = self.next_seq();
        self.observe(seq, &body);
        let payload = RecordBody { body: Some(body) }.encode_to_vec();
        self.pending_bytes += payload.len();
        self.pending.push(payload);
        Ok(seq)
    }

    /// Очередь записи достигла предела: новые команды не принимаются, пока журнал не
    /// начнёт писать снова.
    pub fn backlog_full(&self) -> bool {
        self.pending_bytes >= self.backlog_limit
    }

    #[cfg(test)]
    pub fn set_backlog_limit(&mut self, limit: usize) {
        self.backlog_limit = limit;
    }

    /// Пишет накопленные записи на диск и недописанное подтверждение. При отказе записи
    /// они остаются в памяти: следующий вызов открывает журнал заново и повторяет запись.
    /// Подтверждение пишется и тогда, когда очередь записей отказывает: оно касается уже
    /// лежащих на диске записей.
    pub fn flush(&mut self) -> Result<(), SupervisorError> {
        let appended = self.append_pending();
        let acked = self.settle_ack();
        appended.and(acked)
    }

    fn append_pending(&mut self) -> Result<(), SupervisorError> {
        if !self.pending.is_empty() {
            self.heal()?;
            let payloads: Vec<&[u8]> = self.pending.iter().map(Vec::as_slice).collect();
            self.journal.append_batch(&payloads)?;
            self.pending.clear();
            self.pending_bytes = 0;
            #[cfg(test)]
            {
                self.commits += 1;
            }
        }
        Ok(())
    }

    fn settle_ack(&mut self) -> Result<(), SupervisorError> {
        if self.owed_ack <= self.journal.acked_seq() {
            return Ok(());
        }
        self.heal()?;
        self.journal.ack(self.owed_ack)?;
        // Компакция идёт до самой ранней ещё нужной записи: хаб подтвердил,
        // а восстановление держится на RequestOpened открытого запроса
        // (и его передаче решения).
        let horizon = self
            .requests
            .values()
            .filter(|request| !request.terminal)
            .map(|request| request.opened_seq)
            .min()
            .unwrap_or_else(|| self.journal.last_seq().saturating_add(1));
        self.journal.compact_up_to(horizon)?;
        Ok(())
    }

    /// Журнал после ошибки I/O отклоняет изменения, пока не открыт заново.
    fn heal(&mut self) -> Result<(), SupervisorError> {
        if !self.journal.needs_reopen() {
            return Ok(());
        }
        let before = self.journal.last_seq();
        self.journal.reopen()?;
        let after = self.journal.last_seq();
        if after < before {
            // Пропали записи, которые уже были выданы: продолжать с тех же номеров нельзя.
            return Err(JournalError::Corrupted {
                from_seq: after + 1,
            }
            .into());
        }
        // Откат оборванного пакета мог не пройти: тогда его начало уже лежит в журнале.
        let written = usize::try_from(after - before)
            .unwrap_or(usize::MAX)
            .min(self.pending.len());
        let drained: usize = self
            .pending
            .drain(..written)
            .map(|payload| payload.len())
            .sum();
        self.pending_bytes -= drained;
        Ok(())
    }

    /// Намерение встаёт в очередь записи и тогда, когда прежние записи не легли на
    /// диск: запись об исходе, которая пойдёт следом, не должна опережать его в журнале.
    pub fn intent(&mut self, body: Body) -> Result<(), SupervisorError> {
        let earlier = self.flush();
        self.append(body)?;
        earlier?;
        self.flush()
    }

    pub fn enable_ship(&mut self, committed: u64) {
        self.ship = Some(Ship {
            next: committed.saturating_add(1),
            inflight: VecDeque::new(),
        });
    }

    pub fn disable_ship(&mut self) {
        self.ship = None;
    }

    pub fn fully_acked(&self) -> bool {
        // Записи, которые ещё не легли на диск, хабу не ушли.
        self.pending.is_empty() && self.journal.acked_seq() >= self.journal.last_seq()
    }

    /// Ack за пределами журнала игнорируется: хаб не может подтвердить ненаписанное.
    pub fn ack(&mut self, up_to: u64) -> Result<(), SupervisorError> {
        if up_to > self.journal.last_seq() {
            return Ok(());
        }
        if let Some(ship) = &mut self.ship {
            while ship.inflight.front().is_some_and(|last| *last <= up_to) {
                ship.inflight.pop_front();
            }
            ship.next = ship.next.max(up_to + 1);
        }
        self.owed_ack = self.owed_ack.max(up_to);
        self.settle_ack()
    }

    pub fn next_batch(&mut self) -> Result<Option<SjBatch>, SupervisorError> {
        let Some(ship) = &mut self.ship else {
            return Ok(None);
        };
        if ship.inflight.len() >= WINDOW {
            return Ok(None);
        }
        let from = ship.next.max(self.journal.first_seq());
        if from > self.journal.last_seq() {
            return Ok(None);
        }
        let mut batch = JournalRecords {
            journal_id: self.journal_id.to_string(),
            records: Vec::new(),
        };
        for record in self.journal.read_from(from, MAX_EVENT_BATCH_BYTES)? {
            if batch.records.len() == MAX_SJ_BATCH_RECORDS {
                break;
            }
            batch.records.push(JournalRecord {
                seq: record.seq,
                body: Some(RecordBody::decode(record.payload.as_slice())?),
            });
            if check_record_batch(&batch).is_err() && batch.records.len() > 1 {
                batch.records.pop();
                break;
            }
        }
        let Some(last) = batch.records.last().map(|record| record.seq) else {
            return Ok(None);
        };
        ship.next = last + 1;
        ship.inflight.push_back(last);
        Ok(Some(SjBatch {
            journal_id: batch.journal_id,
            records: batch.records,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(id: &str, payload_len: usize) -> Body {
        Body::RequestOpened(RequestOpened {
            request_id: id.into(),
            execution_id: "exec".into(),
            generation: 7,
            kind: "permission".into(),
            payload_json: vec![b'x'; payload_len],
        })
    }

    fn filler(seq: usize) -> Body {
        Body::DecisionUnconfirmed(canterlot_proto::worker::v1::DecisionUnconfirmed {
            request_id: format!("filler-{seq}"),
            decision_id: "d".into(),
            reason: "x".repeat(64 * 1024),
        })
    }

    #[test]
    fn next_seq_keeps_growing_across_a_reopen() {
        let root = tempfile::tempdir().unwrap();
        let mut sj = Sj::create(root.path()).unwrap();
        assert_eq!(sj.next_seq(), sj.append(filler(0)).unwrap());
        assert_eq!(sj.next_seq(), sj.append(filler(1)).unwrap());
        sj.flush().unwrap();
        let last = sj.last_seq();
        drop(sj);
        let reopened = Sj::create(root.path()).unwrap();
        assert_eq!(reopened.next_seq(), last + 1);
    }

    #[test]
    fn compaction_continues_after_requests_and_terminal_payloads_leave_memory() {
        let root = tempfile::tempdir().unwrap();
        let mut sj = Sj::create(root.path()).unwrap();
        // Ранний запрос с тяжёлым текстом, затем закрыт: текст обязан покинуть память.
        sj.append(opened("r1", 64 * 1024)).unwrap();
        sj.flush().unwrap();
        sj.append(Body::RequestClosed(
            canterlot_proto::worker::v1::RequestClosed {
                request_id: "r1".into(),
                reason: "answered".into(),
            },
        ))
        .unwrap();
        sj.flush().unwrap();
        let closed = sj.request("r1").expect("closed request stays known");
        assert!(closed.terminal);
        assert!(
            closed.opened.payload_json.is_empty(),
            "terminal payload must leave memory"
        );
        // Открытый запрос обязан пережить рестарт и повторное объявление.
        // Наполнитель крутит сегменты: записи acknowledged уходят на диск пачками.
        for chunk in 0..2 {
            for index in 0..80 {
                sj.append(filler(chunk * 80 + index)).unwrap();
            }
            sj.flush().unwrap();
        }
        sj.append(opened("r2", 64 * 1024)).unwrap();
        sj.flush().unwrap();
        let last = sj.last_seq();
        assert!(last > 160);
        sj.ack(last).unwrap();
        // Компакция прошла несмотря на непустую карту запросов.
        assert!(
            sj.first_seq() > 2,
            "acked segments below the open request must be compacted"
        );
        assert!(
            sj.journal.bytes_on_disk() < 6 * 1024 * 1024,
            "journal must stay bounded after the request is closed"
        );
        drop(sj);
        let recovered = Sj::create(root.path()).unwrap();
        let open = recovered.open_requests();
        assert_eq!(open.len(), 1, "only the open request is re-announceable");
        assert_eq!(open[0].request_id, "r2");
        assert_eq!(open[0].payload_json.len(), 64 * 1024);
    }

    fn set_writable(dir: &Path, writable: bool) {
        use std::os::unix::fs::PermissionsExt;
        let mode = if writable { 0o700 } else { 0o500 };
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Хаб подтвердил последний пакет, а замена файла ack отказала (диск полон).
    /// Подтверждение не теряется: следующий сброс очереди, даже пустой, дописывает его,
    /// как только хранилище снова пишет, и сопровождение может выйти.
    #[test]
    fn a_failed_ack_write_is_retried_by_the_next_flush() {
        let root = tempfile::tempdir().unwrap();
        let mut sj = Sj::create(root.path()).unwrap();
        sj.append(Body::PhaseChanged(Default::default())).unwrap();
        sj.flush().unwrap();
        let last = sj.last_seq();
        set_writable(&root.path().join("sj"), false);
        assert!(sj.ack(last).is_err());
        assert!(!sj.fully_acked());
        assert!(sj.flush().is_err(), "the ack is still owed");
        set_writable(&root.path().join("sj"), true);
        sj.flush().unwrap();
        assert!(sj.fully_acked());
        drop(sj);
        assert_eq!(Sj::create(root.path()).unwrap().acked_seq(), last);
    }

    #[test]
    fn burst_shares_commit_but_intent_has_its_own_commit() {
        let root = tempfile::tempdir().unwrap();
        let mut sj = Sj::create(root.path()).unwrap();
        for _ in 0..3 {
            sj.append(Body::PhaseChanged(Default::default())).unwrap();
        }
        assert_eq!(sj.journal.last_seq(), 0);
        sj.flush().unwrap();
        assert_eq!((sj.last_seq(), sj.commits), (3, 1));
        sj.append(Body::PhaseChanged(Default::default())).unwrap();
        sj.intent(Body::TransmissionStarted(Default::default()))
            .unwrap();
        assert_eq!((sj.last_seq(), sj.commits), (5, 3));
        assert!(matches!(
            RecordBody::decode(
                sj.journal.read_from(5, usize::MAX).unwrap()[0]
                    .payload
                    .as_slice()
            )
            .unwrap()
            .body,
            Some(Body::TransmissionStarted(_))
        ));
    }
}
