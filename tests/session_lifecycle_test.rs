//! End-to-end coverage of session/authentication behavior through one
//! long-lived client: registration, rejecting bad credentials, the auth
//! wall around authenticated calls, and `logout` being a safe no-op —
//! driven through `fyde-sdk`'s public API against a real server (see
//! `tests/common/mod.rs`).

mod common;

use anyhow::{Context as _, ensure};

use common::{Scenario, TEST_PASSWORD, random_username, write_temp_pdf};

#[tokio::test]
async fn session_lifecycle() {
    let scenario = Scenario::start().await;
    let username = random_username();

    scenario
        .step("uploading before authenticating fails", || async {
            let file = write_temp_pdf("no session yet");
            let err = scenario
                .client
                .documents()
                .upload(file.path())
                .await
                .err()
                .context("upload should fail before any session is open")?;
            ensure!(
                err.to_string().contains("no master key found"),
                "unexpected error before authentication: {err}"
            );
            Ok(())
        })
        .await;

    scenario
        .step("create account", || async {
            let token = scenario
                .client
                .users()
                .create(&username, TEST_PASSWORD, "e2e-session-device")
                .await
                .context("failed to create account")?;
            ensure!(
                !token.is_empty(),
                "create should return a non-empty session token"
            );
            Ok(())
        })
        .await;

    scenario
        .step("registering the same username again fails", || async {
            let err = scenario
                .client
                .users()
                .create(&username, TEST_PASSWORD, "another-device")
                .await
                .err()
                .context("re-registering the same username should fail")?;
            ensure!(
                err.to_string().contains("already"),
                "unexpected error re-registering an existing username: {err}"
            );
            Ok(())
        })
        .await;

    scenario
        .step("uploading once authenticated succeeds", || async {
            let file = write_temp_pdf("has a session now");
            scenario
                .client
                .documents()
                .upload(file.path())
                .await
                .context("upload should succeed once a session is open")?;
            Ok(())
        })
        .await;

    scenario
        .step("logout closes the session", || async {
            scenario
                .client
                .users()
                .logout()
                .await
                .context("failed to log out")?;
            Ok(())
        })
        .await;

    scenario
        .step("uploading after logout fails again", || async {
            let file = write_temp_pdf("session closed");
            let err = scenario
                .client
                .documents()
                .upload(file.path())
                .await
                .err()
                .context("upload should fail once the session is closed")?;
            ensure!(
                err.to_string().contains("no master key found"),
                "unexpected error after logout: {err}"
            );
            Ok(())
        })
        .await;

    scenario
        .step("logging in with the wrong password fails", || async {
            let err = scenario
                .client
                .users()
                .login(
                    &username,
                    "definitely the wrong password",
                    "e2e-session-device",
                )
                .await
                .err()
                .context("login with the wrong password should fail")?;
            ensure!(
                err.to_string().contains("invalid credentials"),
                "unexpected error for a wrong password: {err}"
            );
            Ok(())
        })
        .await;

    scenario
        .step("logging in with an unknown username fails", || async {
            let err = scenario
                .client
                .users()
                .login(&random_username(), TEST_PASSWORD, "e2e-session-device")
                .await
                .err()
                .context("login with an unknown username should fail")?;
            ensure!(
                err.to_string().contains("invalid credentials"),
                "unexpected error for an unknown username: {err}"
            );
            Ok(())
        })
        .await;

    scenario
        .step(
            "logging back in with the right credentials reopens a session",
            || async {
                let token = scenario
                    .client
                    .users()
                    .login(&username, TEST_PASSWORD, "e2e-session-device")
                    .await
                    .context("failed to log back in with the correct credentials")?;
                ensure!(
                    !token.is_empty(),
                    "login should return a non-empty session token"
                );
                Ok(())
            },
        )
        .await;

    scenario
        .step("uploading works again after logging back in", || async {
            let file = write_temp_pdf("session reopened");
            scenario
                .client
                .documents()
                .upload(file.path())
                .await
                .context("upload should succeed again once logged back in")?;
            Ok(())
        })
        .await;

    scenario
        .step("logout", || async {
            scenario
                .client
                .users()
                .logout()
                .await
                .context("failed to log out")?;
            Ok(())
        })
        .await;

    scenario
        .step(
            "logging out again with no open session is a no-op",
            || async {
                scenario
                    .client
                    .users()
                    .logout()
                    .await
                    .context("logging out with no open session should not fail")?;
                Ok(())
            },
        )
        .await;
}
