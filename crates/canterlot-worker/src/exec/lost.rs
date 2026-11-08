//! Потерянное исполнение до выхода из `owner_lost`. Живого сопровождения нет, поэтому здесь
//! свой маленький актор: ждёт пустую группу, остаток `SJ` читает только для чтения и отдаёт
//! хабу, а итог пишет в `CJ` уже после подтверждения остатка.

use std::{
    collections::{HashSet, VecDeque},
    path::Path,
    sync::Arc,
    time::Duration,
};

use canterlot_journal::{Journal, ReadOnlyJournal};
use canterlot_proto::{
    limits::{MAX_EVENT_BATCH_BYTES, MAX_SJ_BATCH_RECORDS, check_record_batch},
    worker::{
        ipc::{Ownership, OwnershipPhase},
        v1::{
            CancelInput, Command, ExecutionState, InputAcceptanceUnconfirmed, InputCancelLost,
            InputCancelWon, InputDispatchDeferred, InputWithdrawn, JournalRecord, JournalRecords,
            RecordBody, WithdrawInput, command::Kind, record_body::Body,
        },
    },
};
use prost::Message;
use tokio::{
    sync::{mpsc, watch},
    time::{Instant, sleep_until},
};
use uuid::Uuid;

use super::{
    ExecError, Shared, SjRelay,
    group::{self, Group},
    manager::{self, Msg, Start},
    open,
};
use crate::session::WorkerError;

const FIRST_CHECK: Duration = Duration::from_millis(250);
const MAX_CHECK: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
pub(super) struct Exit {
    state: &'static str,
    outcome: &'static str,
}

const SUPERVISOR_DIED: Exit = Exit {
    state: "failed",
    outcome: "supervisor_died",
};
const ADMIN_RESOLVED: Exit = Exit {
    state: "failed",
    outcome: "admin_resolved",
};
pub(super) const MACHINE_REBOOTED: Exit = Exit {
    state: "stopped",
    outcome: "machine_rebooted",
};

struct Drain {
    journal: ReadOnlyJournal,
    journal_id: Uuid,
    next: u64,
    inflight: Option<u64>,
}

struct Watch {
    shared: Arc<Shared>,
    id: Uuid,
    record: Ownership,
    rx: mpsc::UnboundedReceiver<Msg>,
    hub_rx: watch::Receiver<Option<Arc<std::collections::HashMap<Uuid, u64>>>>,
    shutdown: watch::Receiver<bool>,
    exit: Option<Exit>,
    drain: Option<Drain>,
    /// Журнал не прочитан целиком: о передачах ввода судить нельзя.
    journal_incomplete: bool,
    outbox: VecDeque<SjRelay>,
    done: bool,
}

/// Заменяет запись актора: команды исполнения идут сюда, пока выход не состоялся.
/// `false`, если наблюдатель не встал.
pub(super) fn watch(shared: &Arc<Shared>, record: Ownership, exit: Option<Exit>) -> bool {
    let Ok(id) = Uuid::parse_str(&record.execution_id) else {
        return false;
    };
    announce_journal(shared, id, &record);
    let (sender, rx) = mpsc::unbounded_channel();
    shared
        .actors
        .lock()
        .expect("actors lock")
        .insert(id, sender);
    let watch = Watch {
        shared: shared.clone(),
        id,
        record,
        rx,
        hub_rx: shared.hub.subscribe(),
        shutdown: shared.shutdown.clone(),
        exit,
        drain: None,
        journal_incomplete: false,
        outbox: VecDeque::new(),
        done: false,
    };
    let task = tokio::spawn(watch.run());
    shared.tasks.lock().expect("tasks lock").push(task);
    true
}

/// Сессия хаба начинает отправку журнала с его курсора и без курсора посчитает остаток
/// разрывом, поэтому журнал объявляется сразу, ещё до рукопожатия.
fn announce_journal(shared: &Shared, id: Uuid, record: &Ownership) {
    let path = Path::new(&record.exec_dir).join("sj");
    let Ok(journal) = Journal::open_read_only(&path) else {
        return;
    };
    let (journal_id, last, acked) = (journal.id().0, journal.last_seq(), journal.acked_seq());
    shared.with_info(id, |info| {
        info.sj_journal_id = Some(journal_id);
        info.last_seq = last;
        info.acked_seq = acked;
        info.lost_journal = true;
    });
}

fn terminal(command: &Command) -> (String, u32) {
    (command.command_id.clone(), command.dispatch_attempt)
}

fn read_batch(
    journal: &ReadOnlyJournal,
    journal_id: Uuid,
    from: u64,
) -> Result<Vec<JournalRecord>, ExecError> {
    let mut batch = JournalRecords {
        journal_id: journal_id.to_string(),
        records: Vec::new(),
    };
    let broken = |_| ExecError::Verify("journal");
    for record in journal
        .read_from(from, MAX_EVENT_BATCH_BYTES)
        .map_err(|_| ExecError::Verify("journal"))?
    {
        if batch.records.len() == MAX_SJ_BATCH_RECORDS {
            break;
        }
        batch.records.push(JournalRecord {
            seq: record.seq,
            body: Some(RecordBody::decode(record.payload.as_slice()).map_err(broken)?),
        });
        if check_record_batch(&batch).is_err() && batch.records.len() > 1 {
            batch.records.pop();
            break;
        }
    }
    Ok(batch.records)
}

/// Итоги неотвеченных команд: сопровождения нет, и за него отвечает управляющая служба.
/// Ввод с начатой передачей и вливание получают только неопределённость: повторять их нельзя.
fn close_commands(
    commands: &[Command],
    transmitted: &HashSet<String>,
    incomplete: bool,
    outcome: &str,
) -> Vec<Body> {
    let started = |id: &str| incomplete || transmitted.contains(id);
    let mut cancelled = HashSet::new();
    let mut withdrawn = HashSet::new();
    for command in commands {
        match &command.kind {
            Some(Kind::CancelInput(CancelInput {
                target_command_id,
                target_dispatch_attempt,
            })) => {
                cancelled.insert((target_command_id.clone(), *target_dispatch_attempt));
            }
            Some(Kind::WithdrawInput(WithdrawInput {
                target_command_id,
                target_dispatch_attempt,
            })) => {
                withdrawn.insert((target_command_id.clone(), *target_dispatch_attempt));
            }
            _ => {}
        }
    }
    commands
        .iter()
        .filter(|command| {
            matches!(
                command.kind,
                Some(Kind::DeliverInput(_) | Kind::SteerInput(_))
            )
        })
        .map(|command| {
            let key = terminal(command);
            let (command_id, dispatch_attempt) = key.clone();
            // Вливание не пишет намерения до записи в CLI: без факта оно могло дойти.
            if matches!(command.kind, Some(Kind::SteerInput(_))) {
                return Body::AcceptanceUnconfirmed(InputAcceptanceUnconfirmed {
                    command_id,
                    dispatch_attempt,
                    reason: outcome.into(),
                });
            }
            if started(&command_id) {
                if cancelled.contains(&key) {
                    return Body::CancelLost(InputCancelLost {
                        command_id,
                        dispatch_attempt,
                        reason: "transmission_started".into(),
                    });
                }
                return Body::AcceptanceUnconfirmed(InputAcceptanceUnconfirmed {
                    command_id,
                    dispatch_attempt,
                    reason: outcome.into(),
                });
            }
            if cancelled.contains(&key) {
                Body::CancelWon(InputCancelWon {
                    command_id,
                    dispatch_attempt,
                })
            } else if withdrawn.contains(&key) {
                Body::InputWithdrawn(InputWithdrawn {
                    command_id,
                    dispatch_attempt,
                })
            } else {
                Body::DispatchDeferred(InputDispatchDeferred {
                    command_id,
                    dispatch_attempt,
                    reason: "execution_stopped".into(),
                })
            }
        })
        .collect()
}

impl Watch {
    async fn run(mut self) {
        if let Err(error) = self.run_inner().await {
            tracing::warn!(execution_id = %self.id, error_kind = ?std::mem::discriminant(&error), "lost execution watch stopped");
            self.shared
                .actors
                .lock()
                .expect("actors lock")
                .remove(&self.id);
        }
    }

    async fn run_inner(&mut self) -> Result<(), ExecError> {
        if self.exit.is_some() {
            self.start_drain().await?;
        }
        let mut delay = FIRST_CHECK;
        let mut check_at = Instant::now();
        let relay = self.shared.relay.clone();
        while !self.done {
            tokio::select! {
                biased;
                _ = self.shutdown.changed() => return Ok(()),
                changed = self.hub_rx.changed() => {
                    if changed.is_ok() {
                        self.on_hub().await?;
                    }
                }
                message = self.rx.recv() => match message {
                    Some(Msg::Command(command)) => self.on_command(command).await?,
                    Some(Msg::SjAck(up_to)) => self.on_ack(up_to).await?,
                    Some(Msg::Replay(committed)) => self.restart_from(committed).await?,
                    Some(_) => {}
                    None => return Ok(()),
                },
                permit = relay.reserve(), if !self.outbox.is_empty() => {
                    let permit = permit.map_err(|_| ExecError::Shutdown)?;
                    if let Some(batch) = self.outbox.pop_front() {
                        permit.send(batch);
                    }
                }
                () = sleep_until(check_at), if self.exit.is_none() => {
                    let group = group::confirmed(&self.record).await;
                    tracing::debug!(execution_id = %self.id, ?group, next_ms = delay.as_millis() as u64, "lost execution group check");
                    if group == Group::Empty {
                        self.begin_exit(SUPERVISOR_DIED).await?;
                    } else {
                        tracing::debug!(execution_id = %self.id, kind = "lost_group_not_empty", "lost execution keeps waiting");
                        check_at = Instant::now() + delay;
                        delay = (delay * 2).min(MAX_CHECK);
                    }
                }
            }
        }
        Ok(())
    }

    async fn on_command(&mut self, command: Command) -> Result<(), ExecError> {
        if matches!(command.kind, Some(Kind::ResolveOwnerLost(_))) {
            return self.resolve(command).await;
        }
        manager::answer_without_execution(&self.shared, &command).await
    }

    /// Решение владельца. Сначала проверяется, не живо ли то же сопровождение: тогда сессия
    /// возвращается целой, и завершать нечего.
    async fn resolve(&mut self, command: Command) -> Result<(), ExecError> {
        if self.exit.is_some() {
            return Ok(());
        }
        if command.execution_generation != self.record.generation {
            return self.shared.cj(vec![], vec![terminal(&command)]).await;
        }
        let started = Instant::now();
        let verified = manager::verify(&self.shared, &self.record).await;
        tracing::debug!(
            execution_id = %self.id,
            verified = verified.is_ok(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "resolve: verify of the lost supervisor"
        );
        match verified {
            Ok(linked) => return self.adopt(linked, command).await,
            Err(error) => {
                tracing::info!(execution_id = %self.id, reason = %error, "lost ownership not reclaimed");
            }
        }
        let grace = self.shared.config.supervisor.stop_grace;
        let group = group::terminate(&self.record, grace).await;
        tracing::debug!(
            execution_id = %self.id,
            ?group,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "resolve: group terminated"
        );
        tracing::info!(execution_id = %self.id, confirmed = (group == Group::Empty), kind = "owner_lost_resolved", "lost group terminated by owner");
        self.begin_exit(if group == Group::Empty {
            SUPERVISOR_DIED
        } else {
            ADMIN_RESOLVED
        })
        .await
    }

    async fn adopt(
        &mut self,
        linked: super::spawn::Linked,
        command: Command,
    ) -> Result<(), ExecError> {
        manager::reclaim_owner(&self.shared, self.id, self.record.generation).await?;
        self.shared.cj(vec![], vec![terminal(&command)]).await?;
        let id = self.id;
        let record = self
            .shared
            .store
            .call(move |store| Ok(open(store)?.ownership().get(id).cloned()))
            .await?
            .ok_or(ExecError::Store(WorkerError::Execution))?;
        tracing::info!(execution_id = %id, kind = "owner_reclaimed", "lost ownership reclaimed");
        self.shared.with_info(id, |info| {
            info.state = if record.cli_pid != 0 {
                "running".into()
            } else {
                "starting".into()
            };
            info.outcome.clear();
            info.retired = false;
            info.cj_terminal_seq = None;
            info.lost_journal = false;
        });
        manager::spawn_actor(
            &self.shared,
            id,
            Start::Reattach(Box::new((linked, record))),
        );
        self.rx.close();
        let sender = self
            .shared
            .actors
            .lock()
            .expect("actors lock")
            .get(&id)
            .cloned();
        let execution = id.to_string();
        let stored = self
            .shared
            .store
            .call(move |store| {
                Ok(open(store)?
                    .stored_commands()?
                    .into_iter()
                    .filter(|command| command.execution_id == execution)
                    .collect::<Vec<_>>())
            })
            .await?;
        if let Some(sender) = sender {
            for command in stored {
                let _ = sender.send(Msg::Command(command));
            }
        }
        self.done = true;
        Ok(())
    }

    async fn begin_exit(&mut self, exit: Exit) -> Result<(), ExecError> {
        tracing::info!(execution_id = %self.id, outcome = exit.outcome, kind = "owner_lost_exit", "lost execution exits");
        self.exit = Some(exit);
        self.start_drain().await
    }

    async fn start_drain(&mut self) -> Result<(), ExecError> {
        let path = Path::new(&self.record.exec_dir).join("sj");
        let journal = match Journal::open_read_only(&path) {
            Ok(journal) => journal,
            Err(_) if !path.exists() => return self.finish().await,
            Err(error) => {
                tracing::warn!(execution_id = %self.id, error_kind = ?std::mem::discriminant(&error), kind = "lost_journal_unreadable", "lost supervisor journal unreadable");
                self.journal_incomplete = true;
                return self.finish().await;
            }
        };
        let journal_id = journal.id().0;
        let (last, acked) = (journal.last_seq(), journal.acked_seq());
        self.shared.with_info(self.id, |info| {
            info.sj_journal_id = Some(journal_id);
            info.last_seq = last;
            info.acked_seq = acked;
            info.lost_journal = true;
        });
        self.drain = Some(Drain {
            journal,
            journal_id,
            next: acked + 1,
            inflight: None,
        });
        self.on_hub().await
    }

    /// Новая сессия хаба называет свою точку продолжения; без сессии ждём её.
    async fn on_hub(&mut self) -> Result<(), ExecError> {
        let Some(journal_id) = self.drain.as_ref().map(|drain| drain.journal_id) else {
            return Ok(());
        };
        let Some(map) = self.hub_rx.borrow_and_update().clone() else {
            return Ok(());
        };
        let committed = map.get(&journal_id).copied();
        let acked = self
            .drain
            .as_ref()
            .map_or(0, |drain| drain.next.saturating_sub(1));
        self.restart_from(committed.unwrap_or(acked)).await
    }

    async fn restart_from(&mut self, committed: u64) -> Result<(), ExecError> {
        self.outbox.clear();
        let Some(drain) = &mut self.drain else {
            return Ok(());
        };
        drain.inflight = None;
        drain.next = committed + 1;
        self.send_next().await
    }

    async fn on_ack(&mut self, up_to: u64) -> Result<(), ExecError> {
        let Some(drain) = &mut self.drain else {
            return Ok(());
        };
        let acked = drain.inflight.is_some_and(|last| last <= up_to);
        if acked {
            drain.inflight = None;
            drain.next = drain.next.max(up_to + 1);
            self.shared
                .with_info(self.id, |info| info.acked_seq = info.acked_seq.max(up_to));
            self.send_next().await?;
        }
        Ok(())
    }

    async fn send_next(&mut self) -> Result<(), ExecError> {
        let Some(drain) = &mut self.drain else {
            return Ok(());
        };
        if drain.inflight.is_some() {
            return Ok(());
        }
        let last_seq = drain.journal.last_seq();
        if drain.next > last_seq {
            return self.finish().await;
        }
        let from = drain.next.max(drain.journal.first_seq());
        let records = match read_batch(&drain.journal, drain.journal_id, from) {
            Ok(records) if !records.is_empty() => records,
            _ => {
                tracing::warn!(execution_id = %self.id, kind = "lost_journal_unreadable", "lost supervisor journal unreadable");
                self.journal_incomplete = true;
                return self.finish().await;
            }
        };
        let last = records.last().map_or(from, |record| record.seq);
        drain.next = last + 1;
        drain.inflight = Some(last);
        let journal_id = drain.journal_id;
        manager::store_relayed(&self.shared, self.id, &records).await?;
        self.outbox.push_back(SjRelay {
            journal_id,
            records,
        });
        Ok(())
    }

    /// Журнал отдан: итог и освобождение одной записью на хранилище. Сбой между ними лишь
    /// повторит итог, хаб примет его как уже применённый.
    async fn finish(&mut self) -> Result<(), ExecError> {
        let Some(exit) = self.exit else {
            return Ok(());
        };
        self.drain = None;
        let (id, generation, incomplete) =
            (self.id, self.record.generation, self.journal_incomplete);
        let (seq, state, record) = self
            .shared
            .store
            .call(move |store| {
                let store = open(store)?;
                let execution = id.to_string();
                let commands: Vec<Command> = store
                    .stored_commands()?
                    .into_iter()
                    .filter(|command| command.execution_id == execution)
                    .collect();
                let mut transmitted = HashSet::new();
                for command in &commands {
                    if store
                        .command_state(&command.command_id)?
                        .is_some_and(|state| state.transmitted)
                    {
                        transmitted.insert(command.command_id.clone());
                    }
                }
                for fact in close_commands(&commands, &transmitted, incomplete, exit.outcome) {
                    store.append_cj_fact(fact)?;
                }
                for command in &commands {
                    store.mark_terminal(&command.command_id, command.dispatch_attempt)?;
                }
                let state = ExecutionState {
                    execution_id: execution,
                    generation,
                    state: exit.state.into(),
                    outcome: exit.outcome.into(),
                    native_conversation_id: String::new(),
                };
                let seq = store.append_cj_fact(Body::ExecutionState(state.clone()))?;
                let mut record = store
                    .ownership()
                    .get(id)
                    .cloned()
                    .ok_or(WorkerError::Execution)?;
                record.phase = OwnershipPhase::Released as i32;
                record.released_reason = exit.outcome.into();
                store.ownership_mut()?.append(record.clone())?;
                Ok((seq, state, record))
            })
            .await?;
        self.shared.remember_cj_states(vec![(seq, state)]);
        self.shared.remember_ownership(&record);
        self.shared.with_info(id, |info| {
            info.lost_journal = false;
            info.attached = false;
        });
        self.shared.actors.lock().expect("actors lock").remove(&id);
        if let Err(error) = manager::cleanup_released(&self.shared).await {
            tracing::warn!(execution_id = %id, error_kind = ?std::mem::discriminant(&error), "lost execution directory not removed");
        }
        self.done = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canterlot_proto::worker::v1::{DeliverInput, SteerInput};

    fn command(id: &str, kind: Kind) -> Command {
        Command {
            command_id: id.into(),
            dispatch_attempt: 1,
            kind: Some(kind),
            ..Default::default()
        }
    }

    fn deliver(id: &str) -> Command {
        command(id, Kind::DeliverInput(DeliverInput::default()))
    }

    #[test]
    fn started_transmission_stays_undetermined_and_the_rest_returns_to_the_queue() {
        let commands = [deliver("sent"), deliver("waiting")];
        let sent = HashSet::from(["sent".to_owned()]);
        let facts = close_commands(&commands, &sent, false, "supervisor_died");
        assert!(
            matches!(&facts[0], Body::AcceptanceUnconfirmed(f) if f.command_id == "sent" && f.reason == "supervisor_died")
        );
        assert!(
            matches!(&facts[1], Body::DispatchDeferred(f) if f.command_id == "waiting" && f.reason == "execution_stopped")
        );
    }

    #[test]
    fn unread_journal_proves_no_input_stayed_on_the_machine() {
        let commands = [deliver("unknown")];
        let facts = close_commands(&commands, &HashSet::new(), true, "supervisor_died");
        assert!(matches!(&facts[0], Body::AcceptanceUnconfirmed(_)));
    }

    #[test]
    fn cancel_and_withdraw_of_an_untransmitted_input_win() {
        let target = |id: &str| (id.to_owned(), 1);
        let commands = [
            deliver("cancelled"),
            deliver("withdrawn"),
            deliver("sent"),
            command(
                "c1",
                Kind::CancelInput(CancelInput {
                    target_command_id: target("cancelled").0,
                    target_dispatch_attempt: 1,
                }),
            ),
            command(
                "c2",
                Kind::WithdrawInput(WithdrawInput {
                    target_command_id: target("withdrawn").0,
                    target_dispatch_attempt: 1,
                }),
            ),
            command(
                "c3",
                Kind::CancelInput(CancelInput {
                    target_command_id: target("sent").0,
                    target_dispatch_attempt: 1,
                }),
            ),
        ];
        let sent = HashSet::from(["sent".to_owned()]);
        let facts = close_commands(&commands, &sent, false, "supervisor_died");
        assert!(matches!(&facts[0], Body::CancelWon(_)));
        assert!(matches!(&facts[1], Body::InputWithdrawn(_)));
        assert!(matches!(&facts[2], Body::CancelLost(_)));
    }

    #[test]
    fn steer_without_a_fact_is_undetermined() {
        let commands = [command("steer", Kind::SteerInput(SteerInput::default()))];
        for incomplete in [false, true] {
            let facts = close_commands(&commands, &HashSet::new(), incomplete, "supervisor_died");
            assert!(
                matches!(&facts[..], [Body::AcceptanceUnconfirmed(f)] if f.command_id == "steer" && f.reason == "supervisor_died"),
                "{facts:?}"
            );
        }
    }
}
