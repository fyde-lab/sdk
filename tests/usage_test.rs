//! End-to-end coverage of the account usage report: a freshly registered
//! account sees its plan's limits and its own usage, which grows as it
//! stores documents, driven through
//! `fyde-sdk`'s public API against a real server (see `tests/common/mod.rs`).

mod common;

use anyhow::{Context as _, ensure};
use fyde_sdk::{Client, ClientConfig, LogLevel, Storage, UploadRequest};
use serial_test::serial;

use common::{TEST_PASSWORD, TestServer, random_username, write_temp_pdf};

async fn client(server: &TestServer) -> Client {
    Client::init(ClientConfig {
        url: server.url().to_string(),
        storage: Storage::Memory,
        log_level: LogLevel::Off,
        on_log: None,
        on_document_change: None,
        on_scraper_progress: None,
        on_scraper_question: None,
    })
    .await
    .expect("failed to connect an sdk client to the test server")
}

// `#[serial(e2e)]`: this suite must not run concurrently with any other
// `tests/` suite — see the sdk CLAUDE.md's "must run serially" note under
// Testing.
#[tokio::test]
#[serial(e2e)]
async fn an_account_reports_its_plan_and_usage() {
    let server = TestServer::start();
    let device = client(&server).await;

    async {
        // The first account ever registered is the admin's: register a
        // second one, on the free plan.
        device
            .users()
            .create(&random_username(), TEST_PASSWORD, "admin-device", false)
            .await
            .context("failed to create the admin account")?;
        device
            .users()
            .logout()
            .await
            .context("failed to log out the admin")?;
        device
            .users()
            .create(&random_username(), TEST_PASSWORD, "laptop", false)
            .await
            .context("failed to create the account")?;

        let usage = device
            .usage()
            .get_usage()
            .await
            .context("failed to get the account's usage")?;

        ensure!(
            usage.plan_id() == "free",
            "unexpected plan {}",
            usage.plan_id()
        );
        ensure!(usage.max_sessions() == 1, "unexpected max_sessions");
        ensure!(usage.max_scripts() == 5, "unexpected max_scripts");
        ensure!(usage.max_size() == 5 * 1024 * 1024, "unexpected max_size");
        ensure!(usage.open_sessions() == 1, "unexpected open_sessions");
        ensure!(usage.enabled_scripts() == 0, "unexpected enabled_scripts");
        ensure!(usage.total_size() == 0, "unexpected total_size");

        let file = write_temp_pdf("counted against the plan");
        device
            .documents()
            .upload(UploadRequest::from_path(file.path()))
            .await
            .context("failed to upload a document")?;
        let usage = device
            .usage()
            .get_usage()
            .await
            .context("failed to get the account's usage after an upload")?;
        ensure!(usage.total_size() > 0, "the upload wasn't counted");

        anyhow::Ok(())
    }
    .await
    .unwrap();
}
