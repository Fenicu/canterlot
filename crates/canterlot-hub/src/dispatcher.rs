use sqlx::PgPool;
use tokio::sync::{broadcast, mpsc, watch};
use uuid::Uuid;

use crate::{
    config::HubTimers,
    delivery::{
        dispatch::{GateInputs, dispatch_next},
        submit::{self, SubmitError, SubmitInput, SubmittedInput},
    },
    session::{Availability, Registry},
};

#[derive(Clone)]
pub struct Dispatcher {
    tx: mpsc::Sender<Uuid>,
}

impl Dispatcher {
    pub fn channel() -> (Self, mpsc::Receiver<Uuid>) {
        let (tx, rx) = mpsc::channel(256);
        (Self { tx }, rx)
    }

    pub fn trigger(&self, conversation_id: Uuid) {
        // При переполнении сигнал восстановит периодический обход PG.
        let _ = self.tx.try_send(conversation_id);
    }

    pub async fn submit_input(
        &self,
        pool: &PgPool,
        request: SubmitInput,
    ) -> Result<SubmittedInput, SubmitError> {
        let conversation = request.conversation_id;
        let submitted = submit::submit_input(pool, request).await?;
        self.trigger(conversation);
        Ok(submitted)
    }
}

pub async fn run(
    pool: PgPool,
    registry: Registry,
    mut rx: mpsc::Receiver<Uuid>,
    timers: HubTimers,
    mut shutdown: watch::Receiver<bool>,
) {
    if *shutdown.borrow() {
        return;
    }
    let mut changes = registry.subscribe_changes();
    let mut sweep = tokio::time::interval(timers.dispatch_sweep);
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        let machine = tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => break,
            conversation = rx.recv(), if !rx.is_closed() || !rx.is_empty() => {
                if let Some(conversation) = conversation {
                    tokio::select! {
                        biased;
                        _ = wait_for_shutdown(&mut shutdown) => break,
                        _ = dispatch(&pool, &registry, conversation) => {},
                    }
                }
                continue;
            },
            change = changes.recv() => match change {
                Ok((machine, Availability::Available)) => Some(machine),
                Err(broadcast::error::RecvError::Lagged(_)) => None,
                _ => continue,
            },
            _ = sweep.tick() => None,
        };
        tokio::select! {
            biased;
            _ = wait_for_shutdown(&mut shutdown) => break,
            _ = async {
                if let Ok(conversations) = queued_conversations(&pool, machine).await {
                    for conversation in conversations {
                        dispatch(&pool, &registry, conversation).await;
                    }
                }
            } => {},
        }
    }
}

pub(crate) async fn wait_for_shutdown(shutdown: &mut watch::Receiver<bool>) {
    let _ = shutdown.wait_for(|stopped| *stopped).await;
}

pub(crate) async fn queued_conversations(
    pool: &PgPool,
    machine: Option<Uuid>,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "select distinct i.conversation_id from inputs i
         join conversations c on c.id = i.conversation_id
         where i.state = 'queued' and ($1::uuid is null or c.machine_id = $1)",
    )
    .bind(machine)
    .fetch_all(pool)
    .await
}

async fn dispatch(pool: &PgPool, registry: &Registry, conversation: Uuid) {
    let machine: Result<Uuid, _> =
        sqlx::query_scalar("select machine_id from conversations where id = $1")
            .bind(conversation)
            .fetch_one(pool)
            .await;
    if let Ok(machine) = machine
        && dispatch_next(
            pool,
            conversation,
            GateInputs {
                machine_available: registry.availability(machine) == Availability::Available,
                machine_reconciled: registry.is_reconciled(machine),
            },
            Uuid::now_v7(),
        )
        .await
        .is_err()
    {
        eprintln!("dispatcher_database_error");
    }
}
