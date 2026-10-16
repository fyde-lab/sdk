//! End-to-end coverage that a session persisted by a previous `Client`
//! instance (mirroring a fresh process restart, e.g. after the app was
//! killed and relaunched) resumes changelog sync on its own the next time
//! `Client::init` runs, without an explicit `login`/`create` call — driven
//! through `fyde-sdk`'s public API against a real server (see
//! `tests/common/mod.rs`).

mod common;

use std::time::Duration;

use anyhow::Context as _;
use fyde_sdk::{Client, ClientConfig, Storage, UploadRequest};
use serial_test::serial;

use common::{TEST_PASSWORD, TestServer, random_username, wait_for, write_temp_pdf};

/// A data directory that doesn't exist yet, for `Storage::Disk` to
/// create (with the client's database inside it) on first connect.
fn temp_data_dir() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("fyde-sdk-reconnect-test-{}", uuid::Uuid::now_v7()))
}

fn config(url: &str, data_dir: &std::path::Path) -> ClientConfig {
    ClientConfig {
        url: url.to_string(),
        client_name: "test".to_string(),
        storage: Storage::Disk(data_dir.to_path_buf()),
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
    // `Storage::Disk` keeps secrets in the OS credential store; keep this
    // test off the machine's real keychain (a headless runner may not even
    // have one) while still letting them outlive the first `Client`.
    fyde_sdk::use_in_process_keystore_for_tests().expect("failed to swap in a test keystore");
    let server = TestServer::start();
    let data_dir = temp_data_dir();
    let username = random_username();

    let document_id = {
        let client = Client::init(config(server.url(), &data_dir))
            .await
            .expect("failed to connect the first client");

        client
            .users()
            .create(&username, TEST_PASSWORD, "device-one", false)
            .await
            .context("failed to create account")
            .unwrap();

        let file = write_temp_pdf("persisted across a reconnect");
        client
            .documents()
            .upload(UploadRequest::from_path(file.path()))
            .await
            .context("failed to upload document")
            .unwrap()

        // `client` is dropped here, without calling `logout` — mirrors the
        // process exiting while a session is still open.
    };

    let client = Client::init(config(server.url(), &data_dir))
        .await
        .expect("failed to reconnect a second client against the same local database");

    // No `login`/`create` call on this second client: `Client::init` must
    // have started sync on its own, since a session was already persisted
    // by the first client.
    let document = wait_for(Duration::from_secs(1), || async {
        client.documents().get(document_id).await.ok()?
    })
    .await;

    assert_eq!(document.id(), document_id);

    std::fs::remove_dir_all(&data_dir).ok();
}
