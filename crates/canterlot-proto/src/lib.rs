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

pub mod harness;
pub mod limits;
pub mod requests;

mod redacted_debug;
