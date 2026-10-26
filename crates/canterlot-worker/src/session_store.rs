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
