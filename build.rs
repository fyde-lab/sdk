fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_prost_build::compile_protos("../api-protos/changelog/v1/changelog.proto")?;
    tonic_prost_build::compile_protos("../api-protos/users/v1/users.proto")?;
    Ok(())
}
