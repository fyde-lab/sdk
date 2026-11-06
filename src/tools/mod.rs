//! Cross-domain building blocks that aren't a domain of their own: logging,
//! hex encoding, the Lua sandbox, the local SQLite client, the client's own
//! directory, the on-device models (their files and their runtime) and test
//! fixtures.

pub(crate) mod client_dir;
pub(crate) mod hex;
pub(crate) mod log;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "foundation for the on-device document analysis to come"
    )
)]
pub(crate) mod models;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "foundation for the on-device document analysis to come"
    )
)]
pub(crate) mod onnx;
pub(crate) mod sandbox;
pub(crate) mod sql;
#[cfg(any(test, feature = "testing"))]
pub(crate) mod testing;
