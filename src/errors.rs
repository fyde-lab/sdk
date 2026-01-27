use super::storage;
use std::io;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("failed to setup the config directory: {0}")]
    ConfigSetupError(#[source] io::Error),

    #[error("failed to init the storage: {0}")]
    InitStorageError(#[source] storage::Error),
}
