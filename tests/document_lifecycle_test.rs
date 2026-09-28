//! End-to-end coverage of a document's life through one long-lived client:
//! upload, background sync into the local cache, rename, listing, and
//! surviving a logout/login cycle — all driven through `fyde-sdk`'s public
//! API against a real server (see `tests/common/mod.rs`).

mod common;

use std::cell::{Cell, RefCell};
use std::time::Duration;

use anyhow::{Context as _, ensure};
use serial_test::serial;
use uuid::Uuid;

use common::{Scenario, TEST_PASSWORD, build_pdf, random_username, wait_for};

// `#[serial(e2e)]`: this suite must not run concurrently with
// `session_lifecycle_test.rs` — see the sdk CLAUDE.md's "must run
// serially" note under Testing.
#[tokio::test]
#[serial(e2e)]
async fn document_lifecycle() {
    let scenario = Scenario::start().await;
    let username = random_username();
    // Shared via interior mutability, not a plain `let mut`, since each
    // step below is its own closure and later steps need to read what an
    // earlier one produced (the id `upload` generates, the original name
    // `rename` must leave untouched).
    let document_id = Cell::new(Uuid::nil());
    let original_name = RefCell::new(String::new());

    scenario
        .step("create account", || async {
            scenario
                .client
                .users()
                .create(&username, TEST_PASSWORD, "e2e-test-device")
                .await
                .context("failed to create account")?;
            Ok(())
        })
        .await;

    // `create` above already started the background changelog sync job;
    // every later step polls `get`/`list` (which only ever read the local
    // cache that job populates) rather than awaiting anything directly.

    scenario
        .step("upload document", || async {
            let file = common::write_temp_pdf("hello end-to-end test");
            let id = scenario
                .client
                .documents()
                .upload(file.path())
                .await
                .context("failed to upload document")?;
            document_id.set(id);
            Ok(())
        })
        .await;

    scenario
        .step("sync makes the document readable locally", || async {
            let id = document_id.get();
            let document = wait_for(Duration::from_secs(1), || async {
                scenario.client.documents().get(id).await.ok()?
            })
            .await;

            ensure!(document.id() == id, "synced document has the wrong id");
            ensure!(
                document.content() == build_pdf("hello end-to-end test").as_slice(),
                "synced document content doesn't match what was uploaded"
            );
            ensure!(
                document.metadata().content_type() == "application/pdf",
                "unexpected content type"
            );
            ensure!(
                document
                    .metadata()
                    .transcript()
                    .contains("hello end-to-end test"),
                "transcript missing the uploaded text"
            );
            *original_name.borrow_mut() = document.metadata().original_name().to_string();
            Ok(())
        })
        .await;

    scenario
        .step("rename document", || async {
            let document = scenario
                .client
                .documents()
                .get(document_id.get())
                .await
                .context("failed to read document before renaming")?
                .context("document missing from local cache before renaming")?;

            let metadata = fyde_sdk::Metadata {
                name: "renamed.pdf".to_string(),
                ..document.metadata().clone()
            };
            scenario
                .client
                .documents()
                .update_metadata(metadata)
                .await
                .context("failed to rename document")?;
            Ok(())
        })
        .await;

    scenario
        .step("sync makes the rename visible locally", || async {
            let id = document_id.get();
            let renamed = wait_for(Duration::from_secs(1), || async {
                let document = scenario.client.documents().get(id).await.ok()??;
                (document.metadata().name() == "renamed.pdf").then_some(document)
            })
            .await;

            ensure!(
                renamed.metadata().original_name() == original_name.borrow().as_str(),
                "renaming must leave original_name untouched"
            );
            Ok(())
        })
        .await;

    scenario
        .step("list includes the uploaded document", || async {
            let id = document_id.get();
            let mut seen = false;
            let mut offset = 0i64;
            loop {
                let page = scenario
                    .client
                    .documents()
                    .list(offset, 50)
                    .await
                    .context("failed to list documents")?;
                if page.is_empty() {
                    break;
                }
                offset += page.len() as i64;
                if page.iter().any(|document| document.id() == id) {
                    seen = true;
                    break;
                }
            }
            ensure!(seen, "uploaded document missing from list");
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
        .step("logout clears the local document cache", || async {
            let document = scenario
                .client
                .documents()
                .get(document_id.get())
                .await
                .context("failed to read local cache after logout")?;
            ensure!(
                document.is_none(),
                "document should no longer be cached locally after logout"
            );
            Ok(())
        })
        .await;

    scenario
        .step("login again", || async {
            scenario
                .client
                .users()
                .login(&username, TEST_PASSWORD, "e2e-test-device")
                .await
                .context("failed to log back in")?;
            Ok(())
        })
        .await;

    // `logout` wiped the local changelog cursor along with the document
    // cache, and `login` above already restarted the sync job, so this
    // replays the account's whole history from scratch.
    scenario
        .step("resyncing after login restores the document", || async {
            let id = document_id.get();
            let document = wait_for(Duration::from_secs(1), || async {
                scenario.client.documents().get(id).await.ok()?
            })
            .await;

            ensure!(
                document.content() == build_pdf("hello end-to-end test").as_slice(),
                "restored document content doesn't match the original upload"
            );
            ensure!(
                document.metadata().name() == "renamed.pdf",
                "restored document should keep the renamed name, not just the original upload"
            );
            Ok(())
        })
        .await;
}
