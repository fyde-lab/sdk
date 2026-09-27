use std::path::Path;
use std::process::Command;

const BUF_MODULE: &str = "buf.build/fyde-lab/api";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::var("OUT_DIR")?;
    let proto_dir = Path::new(&out_dir).join("api-protos");

    let status = Command::new("buf")
        .args(["export", BUF_MODULE, "-o"])
        .arg(&proto_dir)
        .status()
        .map_err(|err| format!("failed to run buf: {err}"))?;
    if !status.success() {
        return Err(format!("buf export {BUF_MODULE} failed with status {status}").into());
    }

    tonic_prost_build::compile_protos(proto_dir.join("changelog/v1/changelog.proto"))?;
    tonic_prost_build::compile_protos(proto_dir.join("users/v1/users.proto"))?;
    tonic_prost_build::compile_protos(proto_dir.join("scripts/v1/scripts.proto"))?;

    println!("cargo:rerun-if-changed=build.rs");

    Ok(())
}
