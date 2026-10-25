fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/probe.proto");
    tonic_prost_build::compile_protos("proto/probe.proto")?;
    Ok(())
}
