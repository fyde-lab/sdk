//! End-to-end coverage of an account's session management: a signed-in
//! device lists every session open on its account, its own flagged, and
//! closes another one — the device that held it is logged out on its next
//! call — driven through `fyde-sdk`'s public API against a real server (see
//! `tests/common/mod.rs`).

mod common;

use anyhow::{Context as _, ensure};
use fyde_sdk::{Client, ClientConfig, ErrorCode, LogLevel, LoginOutcome, Storage, UploadRequest};
use serial_test::serial;

use common::{TEST_PASSWORD, TestServer, random_username, write_temp_pdf};

async fn client(server: &TestServer) -> Client {
    Client::init(ClientConfig {
        url: server.url().to_string(),
        client_name: "test".to_string(),
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
async fn a_device_lists_the_accounts_sessions_and_closes_another_one() {
    let server = TestServer::start();
    let laptop = client(&server).await;
    let phone = client(&server).await;
    let username = random_username();

    async {
        // The first account ever registered is the admin, whose plan allows
        // several sessions at once.
        laptop
            .users()
            .create(&username, TEST_PASSWORD, "laptop", false)
            .await
            .context("failed to create the account")?;
        let outcome = phone
            .users()
            .login(&username, TEST_PASSWORD, "phone", false, None)
            .await
            .context("failed to log the phone in")?;
        ensure!(
            matches!(outcome, LoginOutcome::LoggedIn { .. }),
            "expected the login to open a session, got {outcome:?}"
        );

        let sessions = phone
            .users()
            .list_sessions()
            .await
            .context("failed to list sessions")?;
        let listed: Vec<_> = sessions
            .iter()
            .map(|session| (session.device_name(), session.is_current()))
            .collect();
        ensure!(
            listed == [("laptop", false), ("phone", true)],
            "unexpected sessions: {sessions:?}"
        );

        phone
            .users()
            .revoke_session(sessions[0].id())
            .await
            .context("failed to close the laptop's session")?;

        let file = write_temp_pdf("uploaded from the laptop");
        let err = laptop
            .documents()
            .upload(UploadRequest::from_path(file.path()))
            .await
            .err()
            .context("upload from the closed session should fail")?;
        ensure!(
            err.code() == ErrorCode::SessionExpired,
            "unexpected error from the closed session: {err}"
        );

        let sessions = phone
            .users()
            .list_sessions()
            .await
            .context("failed to list sessions again")?;
        ensure!(
            sessions.len() == 1 && sessions[0].is_current(),
            "unexpected sessions after closing the laptop's: {sessions:?}"
        );

        let err = phone
            .users()
            .revoke_session(&uuid::Uuid::now_v7().to_string())
            .await
            .err()
            .context("closing an unknown session should fail")?;
        ensure!(
            err.code() == ErrorCode::NotFoundOnServer,
            "unexpected error closing an unknown session: {err}"
        );

        anyhow::Ok(())
    }
    .await
    .unwrap();
}
