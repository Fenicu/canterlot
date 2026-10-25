use std::{future::Future, thread};

use tokio::sync::{mpsc, oneshot};

use crate::{
    session::WorkerError,
    store::{StoreError, WorkerStore},
};

type Job = Box<dyn FnOnce(&mut Option<WorkerStore>) + Send>;

pub(crate) struct StoreThread {
    sender: mpsc::Sender<Job>,
    thread: thread::JoinHandle<()>,
}

impl StoreThread {
    pub(crate) fn new(store: WorkerStore) -> Self {
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

    pub(crate) fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Option<WorkerStore>) -> Result<T, WorkerError> + Send + 'static,
    ) -> impl Future<Output = Result<T, WorkerError>> + Send + 'static {
        let sender = self.sender.clone();
        async move {
            let (reply, result) = oneshot::channel();
            sender
                .send(Box::new(move |store| {
                    let _ = reply.send(operation(store));
                }))
                .await
                .map_err(|_| WorkerError::StoreThread)?;
            result.await.map_err(|_| WorkerError::StoreThread)?
        }
    }

    pub(crate) async fn close(self) -> Result<(), WorkerError> {
        drop(self.sender);
        tokio::task::spawn_blocking(move || self.thread.join())
            .await
            .map_err(|_| WorkerError::StoreThread)?
            .map_err(|_| WorkerError::StoreThread)
    }
}
