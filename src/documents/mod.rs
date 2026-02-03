mod service;
mod storage;

pub use service::{GetAllCmd, Service};

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use sea_query::enum_def;
use serde::Serialize;
use std::boxed::Box;
use std::sync::Mutex;
use storage::SqliteStorage;
use uuid::Uuid;

pub static AUTHORIZED_MIME_TYPES: &[&str] = &["application/pdf"];

#[enum_def]
#[derive(Debug, PartialEq, Serialize)]
#[readonly::make]
pub struct Metadata {
    pub id: Uuid,
    pub name: String,
    pub checksum: String,
    pub detected_type: String,
    pub size: u64,
    pub created_at: DateTime<Utc>,
    pub transcript: Option<String>,
}

#[enum_def]
#[derive(Debug, PartialEq, Serialize)]
#[readonly::make]
pub struct Document {
    pub metadata: Metadata,
    pub file_content: Box<[u8]>,
    pub file_preview: Box<[u8]>,
}

pub fn init(conn: Mutex<Connection>) -> impl Service {
    let storage = SqliteStorage::new(conn);

    service::Svc::new(Box::new(storage))
}
