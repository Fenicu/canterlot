use grpc_probe::{Error, measurement};

#[tokio::main(worker_threads = 4)]
async fn main() -> Result<(), Error> {
    let topology = std::env::args()
        .nth(1)
        .ok_or("expected shared or separate")?;
    let shared = match topology.as_str() {
        "shared" => true,
        "separate" => false,
        _ => return Err("expected shared or separate".into()),
    };
    let result = measurement::run(shared).await?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}
