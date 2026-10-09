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

    // Deliberately no `cargo:rerun-if-changed` here: the protos come from the BSR module
    // above, not from any local file, so there's nothing for Cargo to watch — emitting
    // `rerun-if-changed=build.rs` made Cargo treat a build with unchanged source as fully
    // cached and skip re-fetching, silently compiling against a stale schema whenever the
    // BSR module moved on without this file changing. Emitting no rerun-if directive at all
    // keeps Cargo's default of always rerunning this build script.

    Ok(())
}
