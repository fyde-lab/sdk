fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::compile_protos("../api-protos/changelog.proto")?;
    tonic_prost_build::compile_protos("../api-protos/users.proto")?;
    Ok(())
}
