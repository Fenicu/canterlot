//! Очередь аренды без ввода-вывода: веса, FIFO по времени запроса, преимущество гейта.
//!
//! Строгий FIFO и есть «преимущество писателя»: ожидающий гейт, который не помещается,
//! останавливает выдачу, поэтому сборки, пришедшие после него, его не обгоняют; сборка,
//! пришедшая раньше, получает слот, если помещается.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Весь станок: `check.sh --all`, `check-web.sh --stack`, гейт оркестратора.
    Gate,
    /// Один слот: всё остальное.
    Build,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Gate => "gate",
            Kind::Build => "build",
        }
    }

    pub fn parse(text: &str) -> Option<Kind> {
        match text {
            "gate" => Some(Kind::Gate),
            "build" => Some(Kind::Build),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry<M> {
    pub id: String,
    pub kind: Kind,
    pub weight: u32,
    /// Время запроса по часам клиента: переподключение после рестарта воркера сохраняет место.
    pub requested_at_ms: u64,
    /// Время выдачи; у ожидающих `None`.
    pub since_ms: Option<u64>,
    pub meta: M,
    seq: u64,
}

#[derive(Debug)]
pub struct Scheduler<M> {
    slots: u32,
    holders: Vec<Entry<M>>,
    waiters: Vec<Entry<M>>,
    next_seq: u64,
}

impl<M: Clone> Scheduler<M> {
    pub fn new(slots: u32) -> Self {
        Self {
            slots: slots.max(1),
            holders: Vec::new(),
            waiters: Vec::new(),
            next_seq: 0,
        }
    }

    pub fn slots(&self) -> u32 {
        self.slots
    }

    pub fn weight(&self, kind: Kind) -> u32 {
        match kind {
            Kind::Gate => self.slots,
            Kind::Build => 1,
        }
    }

    pub fn used(&self) -> u32 {
        self.holders.iter().map(|entry| entry.weight).sum()
    }

    pub fn holders(&self) -> &[Entry<M>] {
        &self.holders
    }

    pub fn waiters(&self) -> &[Entry<M>] {
        &self.waiters
    }

    /// Выданная до рестарта аренда: держится как есть, даже если слотов стало меньше.
    pub fn restore(
        &mut self,
        id: String,
        kind: Kind,
        weight: u32,
        since_ms: u64,
        requested_at_ms: u64,
        meta: M,
    ) {
        let seq = self.bump();
        self.holders.push(Entry {
            id,
            kind,
            weight,
            requested_at_ms,
            since_ms: Some(since_ms),
            meta,
            seq,
        });
    }

    /// Ставит запрос в очередь и возвращает выданные этим шагом аренды.
    pub fn request(
        &mut self,
        id: String,
        kind: Kind,
        requested_at_ms: u64,
        now_ms: u64,
        meta: M,
    ) -> Vec<String> {
        let seq = self.bump();
        let entry = Entry {
            id,
            kind,
            weight: self.weight(kind),
            requested_at_ms,
            since_ms: None,
            meta,
            seq,
        };
        let at = self
            .waiters
            .partition_point(|other| (other.requested_at_ms, other.seq) <= (requested_at_ms, seq));
        self.waiters.insert(at, entry);
        self.schedule(now_ms)
    }

    /// Снимает держателя или ожидающего; возвращает выданные после этого аренды.
    pub fn release(&mut self, id: &str, now_ms: u64) -> (Option<Entry<M>>, Vec<String>) {
        let removed = if let Some(at) = self.holders.iter().position(|e| e.id == id) {
            Some(self.holders.remove(at))
        } else {
            self.waiters
                .iter()
                .position(|e| e.id == id)
                .map(|at| self.waiters.remove(at))
        };
        let granted = if removed.is_some() {
            self.schedule(now_ms)
        } else {
            Vec::new()
        };
        (removed, granted)
    }

    /// Место в очереди с единицы; у держателя и неизвестного `None`.
    pub fn position(&self, id: &str) -> Option<usize> {
        self.waiters
            .iter()
            .position(|entry| entry.id == id)
            .map(|at| at + 1)
    }

    pub fn holder(&self, id: &str) -> Option<&Entry<M>> {
        self.holders.iter().find(|entry| entry.id == id)
    }

    fn schedule(&mut self, now_ms: u64) -> Vec<String> {
        let mut granted = Vec::new();
        let mut used = self.used();
        while let Some(head) = self.waiters.first() {
            if used + head.weight > self.slots {
                break;
            }
            let mut entry = self.waiters.remove(0);
            used += entry.weight;
            entry.since_ms = Some(now_ms);
            granted.push(entry.id.clone());
            self.holders.push(entry);
        }
        granted
    }

    fn bump(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum Step {
        Request(&'static str, Kind, u64),
        Release(&'static str),
        Restore(&'static str, Kind, u32),
    }
    use Step::*;

    struct Case {
        name: &'static str,
        slots: u32,
        steps: Vec<Step>,
        holders: Vec<&'static str>,
        waiters: Vec<&'static str>,
    }

    fn run(case: &Case) -> Scheduler<()> {
        let mut scheduler = Scheduler::new(case.slots);
        for (now, step) in case.steps.iter().enumerate() {
            let now = now as u64 * 10;
            match step {
                Request(id, kind, at) => {
                    scheduler.request((*id).into(), *kind, *at, now, ());
                }
                Release(id) => {
                    scheduler.release(id, now);
                }
                Restore(id, kind, weight) => {
                    scheduler.restore((*id).into(), *kind, *weight, now, now, ());
                }
            }
        }
        scheduler
    }

    #[test]
    fn table() {
        let cases = vec![
            Case {
                name: "две сборки занимают два слота, третья ждёт",
                slots: 2,
                steps: vec![
                    Request("b1", Kind::Build, 1),
                    Request("b2", Kind::Build, 2),
                    Request("b3", Kind::Build, 3),
                ],
                holders: vec!["b1", "b2"],
                waiters: vec!["b3"],
            },
            Case {
                name: "гейт исключительный",
                slots: 2,
                steps: vec![Request("g", Kind::Gate, 1), Request("b", Kind::Build, 2)],
                holders: vec!["g"],
                waiters: vec!["b"],
            },
            Case {
                name: "ожидающий гейт не пропускает поздние сборки",
                slots: 2,
                steps: vec![
                    Request("b1", Kind::Build, 1),
                    Request("g", Kind::Gate, 2),
                    Request("b2", Kind::Build, 3),
                ],
                holders: vec!["b1"],
                waiters: vec!["g", "b2"],
            },
            Case {
                name: "сборка, пришедшая раньше гейта, получает слот",
                slots: 3,
                steps: vec![
                    Request("b1", Kind::Build, 1),
                    Request("b2", Kind::Build, 2),
                    Request("b3", Kind::Build, 3),
                    Release("b1"),
                    Request("g", Kind::Gate, 5),
                    // Переподключение после рестарта: запрос со старым временем раньше гейта.
                    Request("b4", Kind::Build, 4),
                ],
                holders: vec!["b2", "b3", "b4"],
                waiters: vec!["g"],
            },
            Case {
                name: "гейт получает станок, когда уходят все сборки",
                slots: 2,
                steps: vec![
                    Request("b1", Kind::Build, 1),
                    Request("b2", Kind::Build, 2),
                    Request("g", Kind::Gate, 3),
                    Request("b3", Kind::Build, 4),
                    Release("b1"),
                    Release("b2"),
                ],
                holders: vec!["g"],
                waiters: vec!["b3"],
            },
            Case {
                name: "после гейта сборки идут по порядку",
                slots: 2,
                steps: vec![
                    Request("g", Kind::Gate, 1),
                    Request("b1", Kind::Build, 2),
                    Request("b2", Kind::Build, 3),
                    Request("b3", Kind::Build, 4),
                    Release("g"),
                ],
                holders: vec!["b1", "b2"],
                waiters: vec!["b3"],
            },
            Case {
                name: "ушедший ожидающий освобождает место в очереди",
                slots: 1,
                steps: vec![
                    Request("b1", Kind::Build, 1),
                    Request("g", Kind::Gate, 2),
                    Request("b2", Kind::Build, 3),
                    Release("g"),
                ],
                holders: vec!["b1"],
                waiters: vec!["b2"],
            },
            Case {
                name: "восстановленные аренды держат слоты сверх ёмкости",
                slots: 2,
                steps: vec![
                    Restore("old-gate", Kind::Gate, 3),
                    Request("b1", Kind::Build, 1),
                ],
                holders: vec!["old-gate"],
                waiters: vec!["b1"],
            },
            Case {
                name: "восстановленная сборка оставляет один слот",
                slots: 2,
                steps: vec![
                    Restore("old", Kind::Build, 1),
                    Request("b1", Kind::Build, 1),
                    Request("b2", Kind::Build, 2),
                ],
                holders: vec!["old", "b1"],
                waiters: vec!["b2"],
            },
            Case {
                name: "одинаковое время — порядок прихода",
                slots: 1,
                steps: vec![
                    Request("b1", Kind::Build, 7),
                    Request("b2", Kind::Build, 7),
                    Request("b3", Kind::Build, 7),
                ],
                holders: vec!["b1"],
                waiters: vec!["b2", "b3"],
            },
            Case {
                name: "освобождение неизвестной аренды ничего не меняет",
                slots: 1,
                steps: vec![Request("b1", Kind::Build, 1), Release("nope")],
                holders: vec!["b1"],
                waiters: vec![],
            },
        ];
        for case in &cases {
            let scheduler = run(case);
            let holders: Vec<_> = scheduler.holders().iter().map(|e| e.id.as_str()).collect();
            let waiters: Vec<_> = scheduler.waiters().iter().map(|e| e.id.as_str()).collect();
            assert_eq!(holders, case.holders, "{}: holders", case.name);
            assert_eq!(waiters, case.waiters, "{}: waiters", case.name);
            assert!(
                scheduler.used() <= scheduler.slots()
                    || case.steps.iter().any(|s| matches!(s, Restore(..))),
                "{}: overcommitted",
                case.name
            );
        }
    }

    #[test]
    fn weights_follow_slots() {
        for (slots, gate, build) in [(1, 1, 1), (2, 2, 1), (3, 3, 1), (0, 1, 1)] {
            let scheduler = Scheduler::<()>::new(slots);
            assert_eq!(scheduler.weight(Kind::Gate), gate, "slots {slots}");
            assert_eq!(scheduler.weight(Kind::Build), build, "slots {slots}");
        }
    }

    #[test]
    fn positions_and_grant_times() {
        let mut scheduler = Scheduler::new(1);
        assert_eq!(
            scheduler.request("a".into(), Kind::Build, 1, 100, ()),
            ["a"]
        );
        assert!(
            scheduler
                .request("g".into(), Kind::Gate, 2, 110, ())
                .is_empty()
        );
        assert!(
            scheduler
                .request("b".into(), Kind::Build, 3, 120, ())
                .is_empty()
        );
        assert_eq!(scheduler.position("a"), None);
        assert_eq!(scheduler.position("g"), Some(1));
        assert_eq!(scheduler.position("b"), Some(2));
        assert_eq!(scheduler.holder("a").unwrap().since_ms, Some(100));
        let (removed, granted) = scheduler.release("a", 200);
        assert_eq!(removed.unwrap().id, "a");
        assert_eq!(granted, ["g"]);
        assert_eq!(scheduler.holder("g").unwrap().since_ms, Some(200));
        assert_eq!(scheduler.position("b"), Some(1));
    }

    #[test]
    fn kind_names_round_trip() {
        for kind in [Kind::Gate, Kind::Build] {
            assert_eq!(Kind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(Kind::parse("exclusive"), None);
    }
}
