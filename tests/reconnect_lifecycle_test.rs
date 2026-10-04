//! End-to-end coverage that a session persisted by a previous `Client`
//! instance (mirroring a fresh process restart, e.g. after the app was
//! killed and relaunched) resumes changelog sync on its own the next time
//! `Client::init` runs, without an explicit `login`/`create` call — driven
//! through `fyde-sdk`'s public API against a real server (see
//! `tests/common/mod.rs`).

mod common;

use std::time::Duration;

use anyhow::Context as _;
use fyde_sdk::{Client, ClientConfig, Storage};
use serial_test::serial;

use common::{TEST_PASSWORD, TestServer, random_username, wait_for, write_temp_pdf};

/// A filesystem path for a SQLite database that doesn't exist yet, for
/// `Storage::Disk` to create on first connect (mirrors
/// `sql::sqlite::tests::temp_db_path`).
fn temp_db_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "fyde-sdk-reconnect-test-{}.db",
        uuid::Uuid::now_v7()
    ))
}

fn config(url: &str, db_path: &std::path::Path) -> ClientConfig {
    ClientConfig {
        url: url.to_string(),
        storage: Storage::Disk(db_path.to_path_buf()),
        log_level: fyde_sdk::LogLevel::Off,
        on_log: None,
        on_document_change: None,
        on_scraper_progress: None,
        on_scraper_question: None,
    }
}

// `#[serial(e2e)]`: this suite must not run concurrently with
// `document_lifecycle_test.rs`/`session_lifecycle_test.rs` — see the sdk
// CLAUDE.md's "must run serially" note under Testing.
#[tokio::test]
#[serial(e2e)]
async fn reconnecting_with_a_persisted_session_resumes_sync_without_logging_in_again() {
    let server = TestServer::start();
    let db_path = temp_db_path();
    let username = random_username();

    let document_id = {
        let client = Client::init(config(server.url(), &db_path))
            .await
            .expect("failed to connect the first client");

        client
            .users()
            .create(&username, TEST_PASSWORD, "device-one")
            .await
            .context("failed to create account")
            .unwrap();

        let file = write_temp_pdf("persisted across a reconnect");
        client
            .documents()
            .upload(file.path())
            .await
            .context("failed to upload document")
            .unwrap()

        // `client` is dropped here, without calling `logout` — mirrors the
        // process exiting while a session is still open.
    };

    let client = Client::init(config(server.url(), &db_path))
        .await
        .expect("failed to reconnect a second client against the same local database");

    // No `login`/`create` call on this second client: `Client::init` must
    // have started sync on its own, since a session was already persisted
    // in the local database from the first client.
    let document = wait_for(Duration::from_secs(1), || async {
        client.documents().get(document_id).await.ok()?
    })
    .await;

    assert_eq!(document.id(), document_id);

    std::fs::remove_file(&db_path).ok();
}
