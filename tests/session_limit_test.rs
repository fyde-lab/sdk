//! End-to-end coverage of the per-plan session limit: a login on an account
//! already at its limit opens no session and lists the open ones instead,
//! and logging in again replacing one of them closes it — the device that
//! held it is logged out on its next call — driven through `fyde-sdk`'s
//! public API against a real server (see `tests/common/mod.rs`).

mod common;

use anyhow::{Context as _, bail, ensure};
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
async fn logging_in_past_the_session_limit_replaces_a_chosen_session() {
    let server = TestServer::start();
    let laptop = client(&server).await;
    let phone = client(&server).await;
    let username = random_username();

    async {
        // The first account ever registered is the admin, on a plan
        // allowing 3 sessions; every later one is on the free plan, allowing
        // a single one — which registering already opens.
        laptop
            .users()
            .create(&random_username(), TEST_PASSWORD, "admin-device", false)
            .await
            .context("failed to create the admin account")?;
        laptop
            .users()
            .logout()
            .await
            .context("failed to log out the admin")?;
        laptop
            .users()
            .create(&username, TEST_PASSWORD, "laptop", false)
            .await
            .context("failed to create the account")?;

        let outcome = phone
            .users()
            .login(&username, TEST_PASSWORD, "phone", false, None)
            .await
            .context("login past the limit should answer, not fail")?;
        let LoginOutcome::SessionLimitReached {
            max_sessions,
            sessions,
        } = outcome
        else {
            bail!("expected the session limit to be reached, got {outcome:?}");
        };
        ensure!(max_sessions == 1, "unexpected max_sessions: {max_sessions}");
        ensure!(
            sessions.len() == 1 && sessions[0].device_name() == "laptop",
            "unexpected open sessions: {sessions:?}"
        );

        let outcome = phone
            .users()
            .login(
                &username,
                TEST_PASSWORD,
                "phone",
                false,
                Some(sessions[0].id()),
            )
            .await
            .context("login replacing the laptop's session should succeed")?;
        ensure!(
            matches!(outcome, LoginOutcome::LoggedIn { .. }),
            "expected the login to open a session, got {outcome:?}"
        );

        let file = write_temp_pdf("uploaded from the phone");
        phone
            .documents()
            .upload(UploadRequest::from_path(file.path()))
            .await
            .context("upload from the new session should succeed")?;

        let file = write_temp_pdf("uploaded from the laptop");
        let err = laptop
            .documents()
            .upload(UploadRequest::from_path(file.path()))
            .await
            .err()
            .context("upload from the replaced session should fail")?;
        ensure!(
            err.code() == ErrorCode::SessionExpired,
            "unexpected error from the replaced session: {err}"
        );

        anyhow::Ok(())
    }
    .await
    .unwrap();
}
