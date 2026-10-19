//! Cross-domain building blocks that aren't a domain of their own: logging,
//! the Lua sandbox, the local SQLite client and test fixtures.

pub(crate) mod log;
pub(crate) mod sandbox;
pub(crate) mod sql;
#[cfg(any(test, feature = "testing"))]
pub(crate) mod testing;
