pub mod worker {
    pub mod v1 {
        tonic::include_proto!("canterlot.worker.v1");
    }
}

pub mod limits;
