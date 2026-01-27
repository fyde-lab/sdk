pub mod documents;
mod errors;
mod storage;

use errors::Error;
use std::boxed::Box;
use std::sync::Mutex;

const APPNAME: &str = "fyde";

pub enum StorageType {
    Memory,
    FileSystem,
}

pub struct SdkConfig {
    pub storage_type: StorageType,
}

pub struct Sdk {
    pub documents: Box<dyn documents::Service>,
}

impl Sdk {
    pub fn init(cfg: &SdkConfig) -> Result<Self, Error> {
        let storage_type = match cfg.storage_type {
            StorageType::Memory => storage::StorageType::Memory,
            StorageType::FileSystem => {
                let xdg_dirs = xdg::BaseDirectories::with_prefix(APPNAME);

                let db_path = xdg_dirs
                    .place_config_file("storage.db3")
                    .map_err(Error::ConfigSetupError)?;

                storage::StorageType::File(&Box::new(db_path))
            }
        };

        let conn = Mutex::new(storage::init(&storage_type).map_err(Error::InitStorageError)?);

        Ok(Sdk {
            documents: Box::new(documents::init(conn)),
        })
    }
}
