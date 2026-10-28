fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::configure()
        .extern_path(".canterlot.worker.v1", "crate::worker::v1")
        .build_server(false)
        .build_client(false)
        .skip_debug([
            ".canterlot.worker.ipc.v1.Deliver",
            ".canterlot.worker.ipc.v1.LocalFile",
            ".canterlot.worker.ipc.v1.Attach",
            ".canterlot.worker.ipc.v1.Ownership",
        ])
        .compile_protos(&["proto/canterlot/worker/v1/ipc.proto"], &["proto"])?;
    tonic_prost_build::configure()
        // Сохраняем форму существующего oneof при аддитивном расширении команд.
        .type_attribute(
            ".canterlot.worker.v1.ControlDown.msg",
            "#[allow(clippy::large_enum_variant)]",
        )
        .skip_debug([
            ".canterlot.worker.v1.TextBlock",
            ".canterlot.worker.v1.DeliverInput",
            ".canterlot.worker.v1.Command",
            ".canterlot.worker.v1.BlockDelta",
            ".canterlot.worker.v1.BlockFinal",
            ".canterlot.worker.v1.BlockStarted",
            ".canterlot.worker.v1.PhaseChanged",
            ".canterlot.worker.v1.AttachmentRef",
            ".canterlot.worker.v1.AttachmentChunk",
            ".canterlot.worker.v1.MediaChunk",
        ])
        .build_server(true)
        .build_client(true)
        .compile_protos(&["proto/canterlot/worker/v1/worker.proto"], &["proto"])?;
    Ok(())
}
