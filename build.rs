use std::path::{Path, PathBuf};
use std::process::Command;

const BUF_MODULE: &str = "buf.build/fyde-lab/api";

/// Points the build at a local checkout of the protos (e.g. `../server/protos`)
/// instead of fetching [`BUF_MODULE`] from the BSR — for offline builds, and
/// for CI or contributors without `buf` or network access to `buf.build`.
const PROTO_DIR_ENV: &str = "FYDE_PROTO_DIR";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_dir = match std::env::var_os(PROTO_DIR_ENV) {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            // Unlike the BSR case below, a local directory is something Cargo
            // can watch, so only rerun when it (or the variable) changes.
            println!("cargo:rerun-if-env-changed={PROTO_DIR_ENV}");
            println!("cargo:rerun-if-changed={}", dir.display());
            dir
        }
        None => export_from_bsr()?,
    };

    tonic_prost_build::compile_protos(proto_dir.join("changelog/v1/changelog.proto"))?;
    tonic_prost_build::compile_protos(proto_dir.join("users/v1/users.proto"))?;
    tonic_prost_build::compile_protos(proto_dir.join("scripts/v1/scripts.proto"))?;
    tonic_prost_build::compile_protos(proto_dir.join("usage/v1/usage.proto"))?;
    tonic_prost_build::compile_protos(proto_dir.join("attributes/v1/attributes.proto"))?;
    tonic_prost_build::compile_protos(proto_dir.join("icons/v1/icons.proto"))?;

    Ok(())
}

/// Exports [`BUF_MODULE`] into `OUT_DIR` with `buf` and returns where it went.
///
/// Deliberately emits no `cargo:rerun-if-changed` on this path: the protos come
/// from the BSR module, not from any local file, so there's nothing for Cargo to
/// watch — emitting `rerun-if-changed=build.rs` made Cargo treat a build with
/// unchanged source as fully cached and skip re-fetching, silently compiling
/// against a stale schema whenever the BSR module moved on without this file
/// changing. Emitting no rerun-if directive at all keeps Cargo's default of
/// always rerunning this build script.
fn export_from_bsr() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let out_dir = std::env::var("OUT_DIR")?;
    let proto_dir = Path::new(&out_dir).join("api-protos");

    let status = Command::new("buf")
        .args(["export", BUF_MODULE, "-o"])
        .arg(&proto_dir)
        .status()
        .map_err(|err| format!("failed to run buf (or set {PROTO_DIR_ENV}): {err}"))?;
    if !status.success() {
        return Err(format!("buf export {BUF_MODULE} failed with status {status}").into());
    }

    Ok(proto_dir)
}
