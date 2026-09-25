use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let contracts = manifest.join("../../contracts");
    let inputs = [
        contracts.join("module/control/v1/control.proto"),
        contracts.join("module/provider/v1/provider.proto"),
        contracts.join("module/sample/v1/sample.proto"),
        contracts.join("capsule/provider/v1/storage.proto"),
        contracts.join("vmm/backend/v1/backend.proto"),
        contracts.join("gateway/v1/gateway.proto"),
    ];
    for input in &inputs {
        println!("cargo:rerun-if-changed={}", input.display());
    }
    let descriptors = protox::compile(inputs, [contracts])?;
    tonic_build::configure()
        .build_client(true)
        .build_server(true)
        .compile_fds(descriptors)?;
    Ok(())
}
