fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .skip_debug([
            ".canterlot.worker.v1.TextBlock",
            ".canterlot.worker.v1.DeliverInput",
            ".canterlot.worker.v1.Command",
        ])
        .build_server(true)
        .build_client(true)
        .compile_protos(&["proto/canterlot/worker/v1/worker.proto"], &["proto"])?;
    Ok(())
}
