#![forbid(unsafe_code)]

pub mod worker {
    pub mod ipc {
        tonic::include_proto!("canterlot.worker.ipc.v1");
    }

    pub mod v1 {
        tonic::include_proto!("canterlot.worker.v1");
    }
}

/// `outcome` факта CJ: управляющая служба заново подтвердила то же живое сопровождение.
pub const OWNER_RECLAIMED: &str = "owner_reclaimed";

pub mod goals;
pub mod harness;
pub mod limits;
pub mod requests;

mod redacted_debug;

/// Запись журнала сессии — свидетельство прогресса хода. Хаб считает по ним
/// молчание (`last_event_at`), воркер — отметку, которую проверяет guarded interrupt:
/// обе стороны обязаны смотреть на один и тот же набор.
pub fn is_stream_progress(body: &worker::v1::record_body::Body) -> bool {
    use worker::v1::record_body::Body;
    matches!(
        body,
        Body::PhaseChanged(_)
            | Body::BlockStarted(_)
            | Body::BlockDelta(_)
            | Body::BlockFinal(_)
            | Body::BlockIncomplete(_)
            | Body::RunFinished(_)
            | Body::TransmissionStarted(_)
            | Body::InputAccepted(_)
            | Body::LimitObserved(_)
            | Body::LimitReached(_)
            | Body::RequestOpened(_)
            | Body::RequestClosed(_)
            | Body::DecisionApplied(_)
            | Body::DecisionNotApplied(_)
            | Body::TodoUpdated(_)
            | Body::BackgroundWorkChanged(_)
            | Body::SessionStuck(_)
            | Body::CompactionEvent(_)
            | Body::ContextProgress(_)
            | Body::CompactionStatus(_)
            | Body::SecretDelivered(_)
            | Body::SecretFailed(_)
            | Body::SecretRemoved(_)
            | Body::SecretRemoveFailed(_)
    )
}
