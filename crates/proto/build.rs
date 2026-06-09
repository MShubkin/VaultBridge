//! Кодогенерация tonic/prost из `proto/signer.proto` (требует `protoc` в окружении).

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure().compile_protos(&["proto/signer.proto"], &["proto"])?;
    println!("cargo:rerun-if-changed=proto/signer.proto");
    Ok(())
}
