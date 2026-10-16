//! End-to-end coverage of attribute definitions: the defaults every user
//! starts with, defining one's own, and uploads being checked against them —
//! all driven through `fyde-sdk`'s public API against a real server (see
//! `tests/common/mod.rs`).

mod common;

use std::collections::BTreeMap;

use anyhow::{Context as _, ensure};
use fyde_sdk::{Attribute, AttributeType, ErrorCode, SetAttributeRequest, UploadRequest};
use serial_test::serial;

use common::{Scenario, TEST_PASSWORD, random_username};

// `#[serial(e2e)]`: this suite must not run concurrently with any other
// `tests/` suite — see the sdk CLAUDE.md's "must run serially" note under
// Testing.
#[tokio::test]
#[serial(e2e)]
async fn attributes_lifecycle() {
    let scenario = Scenario::start().await;
    let username = random_username();

    scenario
        .step("create account", || async {
            scenario
                .client
                .users()
                .create(&username, TEST_PASSWORD, "e2e-test-device", false)
                .await
                .context("failed to create account")?;
            Ok(())
        })
        .await;

    scenario
        .step("a new user lists only the default attributes", || async {
            let definitions = scenario
                .client
                .attributes()
                .list()
                .await
                .context("failed to list attributes")?;

            ensure!(
                definitions.iter().all(|definition| definition.is_default()),
                "a new user already has attributes of its own: {definitions:?}"
            );
            let net_pay = definitions
                .iter()
                .find(|definition| definition.name() == "net_pay")
                .context("net_pay isn't among the default attributes")?;
            ensure!(
                net_pay.r#type() == AttributeType::Amount,
                "net_pay isn't an amount"
            );
            Ok(())
        })
        .await;

    scenario
        .step("a user defines an attribute of its own", || async {
            let definition = scenario
                .client
                .attributes()
                .set(SetAttributeRequest {
                    name: "warranty_end".to_string(),
                    r#type: AttributeType::Date,
                    description: "When the warranty ends".to_string(),
                })
                .await
                .context("failed to set attribute")?;
            ensure!(
                !definition.is_default(),
                "the user's attribute is marked default"
            );

            let definitions = scenario
                .client
                .attributes()
                .list()
                .await
                .context("failed to list attributes")?;
            let last = definitions.last().context("no attributes listed")?;
            ensure!(
                last == &definition,
                "the user's attribute isn't listed after the defaults: {definitions:?}"
            );
            Ok(())
        })
        .await;

    scenario
        .step("a user can't redefine a default attribute", || async {
            let result = scenario
                .client
                .attributes()
                .set(SetAttributeRequest {
                    name: "net_pay".to_string(),
                    r#type: AttributeType::Float,
                    description: String::new(),
                })
                .await;

            ensure!(result.is_err(), "redefining net_pay succeeded");
            Ok(())
        })
        .await;

    scenario
        .step(
            "an upload is checked against the user's definitions",
            || async {
                let file = common::write_temp_pdf("payslip");
                let mut request = UploadRequest::from_path(file.path());
                request.attributes =
                    BTreeMap::from([("warranty_end".to_string(), Attribute::Integer(2027))]);

                let err = scenario
                    .client
                    .documents()
                    .upload(request)
                    .await
                    .err()
                    .context("an upload with a mistyped attribute succeeded")?;
                ensure!(
                    err.code() == ErrorCode::InvalidAttribute,
                    "unexpected error: {err}"
                );

                let mut request = UploadRequest::from_path(file.path());
                request.attributes = BTreeMap::from([(
                    "warranty_end".to_string(),
                    Attribute::Date("2027-03-01".to_string()),
                )]);
                scenario
                    .client
                    .documents()
                    .upload(request)
                    .await
                    .context("failed to upload a document with a well-typed attribute")?;
                Ok(())
            },
        )
        .await;
}
