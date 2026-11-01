use sqlx::{PgPool, postgres::PgListener};
use tokio_util::{sync::CancellationToken, task::AbortOnDropHandle};
use uuid::Uuid;

use crate::{machines, session::Registry};

pub(super) async fn start(pool: PgPool, registry: Registry) -> AbortOnDropHandle<()> {
    let (ready, started) = tokio::sync::oneshot::channel();
    let task = AbortOnDropHandle::new(tokio::spawn(async move {
        let mut ready = Some(ready);
        loop {
            let mut listener =
                match PgListener::connect_with(&crate::db::listener_pool(&pool)).await {
                    Ok(mut listener) => {
                        if listener.listen("canterlot_revoke").await.is_err() {
                            tracing::warn!(
                                kind = "revocation_listener_database",
                                "revocation listener retry"
                            );
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                            continue;
                        }
                        listener
                    }
                    Err(_) => {
                        tracing::warn!(
                            kind = "revocation_listener_database",
                            "revocation listener retry"
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                        continue;
                    }
                };
            if let Some(ready) = ready.take() {
                let _ = ready.send(());
            }
            let mut sweep = tokio::time::interval(std::time::Duration::from_secs(1));
            loop {
                let machine = tokio::select! {
                    notification = listener.recv() => match notification {
                        Ok(notification) => Uuid::parse_str(notification.payload()).ok(),
                        Err(_) => break,
                    },
                    _ = sweep.tick() => None,
                };
                for (id, connection, credential) in registry.credentials() {
                    if machine.is_some_and(|machine| machine != id) {
                        continue;
                    }
                    let active = async {
                        machines::credential_active(&mut *pool.acquire().await?, id, &credential)
                            .await
                    }
                    .await;
                    match active {
                        Ok(false) => registry.revoke(id, connection),
                        Ok(true) => {}
                        Err(_) => {
                            tracing::warn!(kind = "revocation_check_database", machine_id = %id, "revocation check failed")
                        }
                    }
                }
            }
        }
    }));
    let _ = started.await;
    task
}

pub(super) struct SessionLog {
    pub machine_id: Uuid,
    pub connection_id: Uuid,
    pub stream: &'static str,
    pub reason: &'static str,
    pub cancelled: CancellationToken,
    pub accepted: bool,
}

impl SessionLog {
    pub fn error(&mut self, status: tonic::Status) -> tonic::Status {
        self.reason = match status.code() {
            tonic::Code::Unauthenticated => "credential_revoked",
            tonic::Code::DeadlineExceeded => "deadline_exceeded",
            tonic::Code::InvalidArgument => "invalid_argument",
            tonic::Code::ResourceExhausted => "resource_exhausted",
            tonic::Code::FailedPrecondition => "failed_precondition",
            tonic::Code::PermissionDenied => "permission_denied",
            tonic::Code::Internal => "internal",
            tonic::Code::Cancelled => "cancelled",
            tonic::Code::Unavailable => "unavailable",
            _ => "transport_error",
        };
        if !self.accepted {
            tracing::warn!(kind = "session_rejected", reason = self.reason,
                machine_id = %self.machine_id, connection_id = %self.connection_id,
                stream = self.stream, "session rejected");
        }
        status
    }
}

impl Drop for SessionLog {
    fn drop(&mut self) {
        let reason = if self.cancelled.is_cancelled() && self.reason == "stream_end" {
            "session_cancelled"
        } else {
            self.reason
        };
        tracing::info!(kind = "session_closed", reason, machine_id = %self.machine_id,
            connection_id = %self.connection_id, stream = self.stream, "session closed");
    }
}
