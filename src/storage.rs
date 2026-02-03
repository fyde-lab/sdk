use include_dir::{Dir, include_dir};
use rusqlite::Connection;
use rusqlite_migration::Migrations;
use std::path::Path;
use std::sync::LazyLock;
use thiserror::Error;

static MIGRATION_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/migrations");

static MIGRATIONS: LazyLock<Migrations<'static>> =
    LazyLock::new(|| Migrations::from_directory(&MIGRATION_DIR).unwrap());

#[derive(Error, Debug)]
pub enum Error {
    #[error("failed to open the database file: {0}")]
    Open(#[source] rusqlite::Error),

    #[error("failed to apply a pragma: {0}")]
    Pragma(#[source] rusqlite::Error),

    #[error("failed to run the storage migration job: {0}")]
    Migration(#[source] rusqlite_migration::Error),
}

pub enum StorageType<'a> {
    Memory,
    File(&'a Path),
}

pub fn init(storage_type: &StorageType) -> Result<Connection, Error> {
    let mut conn = match storage_type {
        StorageType::Memory => Connection::open_in_memory().map_err(Error::Open)?,
        StorageType::File(path) => Connection::open(path).map_err(Error::Open)?,
    };

    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(Error::Pragma)?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(Error::Pragma)?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(Error::Pragma)?;

    MIGRATIONS.to_latest(&mut conn).map_err(Error::Migration)?;

    Ok(conn)
}

#[test]
fn migrations_test() {
    MIGRATIONS.validate().unwrap()
}
