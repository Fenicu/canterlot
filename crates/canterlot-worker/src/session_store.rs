use std::{future::Future, thread};

use tokio::sync::{mpsc, oneshot};

use crate::{
    session::WorkerError,
    store::{StoreError, WorkerStore},
};

type Job = Box<dyn FnOnce(&mut Option<WorkerStore>) + Send>;

pub struct StoreThread {
    sender: mpsc::Sender<Job>,
    thread: thread::JoinHandle<()>,
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
        let thread = thread::spawn(move || {
            let mut store = Some(store);
            while let Some(job) = receiver.blocking_recv() {
                job(&mut store);
                if store
                    .as_ref()
                    .is_some_and(|s| matches!(s.cj_cursor(), Err(StoreError::NeedsReopen)))
                {
                    store.take();
                }
            }
        });
        Self { sender, thread }
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
