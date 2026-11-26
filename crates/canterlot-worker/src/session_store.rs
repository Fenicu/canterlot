use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
};

use tokio::sync::{Notify, mpsc, oneshot};

use crate::{
    session::WorkerError,
    store::{StoreError, WorkerStore},
};

type Job = Box<dyn FnOnce(&mut Option<WorkerStore>) + Send>;

pub struct StoreThread {
    sender: mpsc::Sender<Job>,
    thread: thread::JoinHandle<()>,
    failed: Arc<AtomicBool>,
    cj_grown: Arc<Notify>,
}

#[derive(Clone)]
pub struct StoreClient {
    sender: mpsc::Sender<Job>,
}

pub(crate) fn store_mut(slot: &mut Option<WorkerStore>) -> Result<&mut WorkerStore, WorkerError> {
    slot.as_mut()
        .ok_or(WorkerError::Store(StoreError::NeedsReopen))
}

async fn submit<T: Send + 'static>(
    sender: mpsc::Sender<Job>,
    operation: impl FnOnce(&mut Option<WorkerStore>) -> Result<T, WorkerError> + Send + 'static,
) -> Result<T, WorkerError> {
    let (reply, result) = oneshot::channel();
    sender
        .send(Box::new(move |store| {
            let _ = reply.send(operation(store));
        }))
        .await
        .map_err(|_| WorkerError::StoreThread)?;
    result.await.map_err(|_| WorkerError::StoreThread)?
}

impl StoreClient {
    pub fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Option<WorkerStore>) -> Result<T, WorkerError> + Send + 'static,
    ) -> impl Future<Output = Result<T, WorkerError>> + Send + 'static {
        submit(self.sender.clone(), operation)
    }
}

impl StoreThread {
    pub fn new(store: WorkerStore) -> Self {
        let (sender, mut receiver) = mpsc::channel::<Job>(8);
        let failed = Arc::new(AtomicBool::new(false));
        let thread_failed = failed.clone();
        let cj_grown = Arc::new(Notify::new());
        let thread_grown = cj_grown.clone();
        let thread = thread::spawn(move || {
            let mut store = Some(store);
            while let Some(job) = receiver.blocking_recv() {
                let before = store.as_ref().map(WorkerStore::cj_last_seq);
                job(&mut store);
                // Факты пишут и задачи вне цикла сеанса: без сигнала они ждали бы его
                // следующего повода, в худшем случае heartbeat.
                if store.as_ref().map(WorkerStore::cj_last_seq) > before {
                    thread_grown.notify_one();
                }
                if store
                    .as_ref()
                    .is_some_and(|s| matches!(s.cj_cursor(), Err(StoreError::NeedsReopen)))
                {
                    store.take();
                    thread_failed.store(true, Ordering::Relaxed);
                }
            }
        });
        Self {
            sender,
            thread,
            failed,
            cj_grown,
        }
    }

    /// Журнал управления вырос: сеансу пора собрать следующий пакет.
    pub(crate) fn cj_grown(&self) -> Arc<Notify> {
        self.cj_grown.clone()
    }

    pub(crate) fn take_failed(&self) -> bool {
        self.failed.swap(false, Ordering::Relaxed)
    }

    pub fn client(&self) -> StoreClient {
        StoreClient {
            sender: self.sender.clone(),
        }
    }

    pub fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Option<WorkerStore>) -> Result<T, WorkerError> + Send + 'static,
    ) -> impl Future<Output = Result<T, WorkerError>> + Send + 'static {
        submit(self.sender.clone(), operation)
    }

    pub async fn close(self) -> Result<(), WorkerError> {
        drop(self.sender);
        tokio::task::spawn_blocking(move || self.thread.join())
            .await
            .map_err(|_| WorkerError::StoreThread)?
            .map_err(|_| WorkerError::StoreThread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_job_on_a_closed_slot_returns_an_error_instead_of_panicking() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = WorkerStore::open(directory.path()).unwrap();
        let thread = StoreThread::new(store);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            thread
                .call(|slot| {
                    slot.take();
                    Ok(())
                })
                .await
                .unwrap();
            let result = thread
                .call(|slot| {
                    store_mut(slot)?.cj_cursor()?;
                    Ok(())
                })
                .await;
            assert!(matches!(
                result,
                Err(WorkerError::Store(StoreError::NeedsReopen))
            ));
            thread.close().await.unwrap();
        });
    }
}
