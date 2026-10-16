use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use uuid::Uuid;

use crate::domains::scripts::{Script, Service as ScriptsService};
use crate::{ErrorContext as _, Result};

use super::{Document, Metadata, PDF_CONTENT_TYPE, Service, transcript, vm};

/// The default [`Service`] implementation: derives a freshly uploaded
/// document's metadata and runs a user's enabled scripts against a
/// document, via an injected [`ScriptsService`].
pub(super) struct ParserClient {
    scripts: Arc<dyn ScriptsService>,
}

impl ParserClient {
    pub(super) fn new(scripts: Arc<dyn ScriptsService>) -> Self {
        Self { scripts }
    }
}

#[async_trait]
impl Service for ParserClient {
    async fn parse_content(&self, content: &[u8], original_name: &str) -> Result<Metadata> {
        let name = Path::new(original_name)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        let doc_transcript =
            transcript::extract(content).context("failed to extract document transcript")?;

        let id = Uuid::now_v7();

        let metadata = Metadata {
            id,
            original_name: original_name.to_string(),
            name,
            content_type: PDF_CONTENT_TYPE.to_string(),
            created_at,
            size: content.len() as u64,
            checksum: super::checksum(content),
            transcript: doc_transcript,
            r#type: None,
            category: None,
            subcategory: None,
            attributes: BTreeMap::new(),
        };

        let document = Document::new(id, content.to_vec(), metadata);
        self.run_scripts(&document).await
    }

    async fn run_scripts(&self, document: &Document) -> Result<Metadata> {
        let enabled = self
            .scripts
            .list_user_scripts()
            .await
            .context("failed to list the scripts enabled for the authenticated user")?;

        // Listings only carry each script's metadata: fetch the content of
        // every one to run.
        let mut scripts = Vec::with_capacity(enabled.len());
        for metadata in enabled {
            let script = self
                .scripts
                .fetch_script(metadata.id())
                .await
                .with_context(|| {
                    format!(
                        "failed to fetch enabled script {:?} ({})",
                        metadata.name(),
                        metadata.id()
                    )
                })?;
            scripts.push(script);
        }

        // Lua runs synchronously and `mlua::Lua` isn't `Send`, so the whole
        // loop — every VM built and dropped inside it — runs on a blocking
        // thread rather than tying up an async worker for as long as the
        // scripts take (bounded by `vm::sandboxed`'s instruction budget).
        let document = document.clone();
        tokio::task::spawn_blocking(move || run_scripts_blocking(&scripts, &document))
            .await
            .context("script runner thread panicked")?
    }
}

/// [`Service::run_scripts`]' synchronous half: runs each of `scripts` in
/// turn against `document`, each in its own fresh [`vm::sandboxed`] VM,
/// threading the metadata one script produced into the next.
fn run_scripts_blocking(scripts: &[Script], document: &Document) -> Result<Metadata> {
    let mut metadata = document.metadata().clone();
    for script in scripts {
        let lua = vm::sandboxed(document.content().len())?;
        let working_document = Document::new(document.id(), document.content().to_vec(), metadata);
        let source_metadata = vm::expose_document(&lua, &working_document)?;
        vm::expose_pdf_conversions(&lua, document.content())?;
        vm::expose_set_category(&lua, source_metadata.clone())?;
        crate::tools::sandbox::load_source(&lua, script.script())
            .exec()
            .with_context(|| {
                format!("failed to run script {:?} ({})", script.name(), script.id())
            })?;

        metadata = vm::read_metadata(&lua, working_document.metadata(), &source_metadata.borrow())?;
    }

    Ok(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::documents::FakeDocument;
    use crate::domains::scripts::{FakeScript, MockService as MockScriptsService, ScriptMetadata};

    /// A [`ScriptsService`] fake with no scripts enabled, so
    /// [`Service::run_scripts`] returns a document's metadata back
    /// unchanged, for tests that don't care about script behavior.
    fn no_op_scripts() -> Arc<dyn ScriptsService> {
        let mut scripts = MockScriptsService::new();
        scripts
            .expect_list_user_scripts()
            .returning(|| Ok(Vec::new()));
        Arc::new(scripts)
    }

    /// A [`ScriptsService`] fake with `scripts` enabled, in that order:
    /// listing them once returns their metadata, and fetching each by id
    /// returns it whole.
    fn enabled_scripts(scripts: Vec<Script>) -> Arc<dyn ScriptsService> {
        let mut mock_scripts = MockScriptsService::new();
        let metadata: Vec<_> = scripts.iter().cloned().map(ScriptMetadata::from).collect();
        mock_scripts
            .expect_list_user_scripts()
            .times(1)
            .return_once(move || Ok(metadata));
        mock_scripts.expect_fetch_script().returning(move |id| {
            Ok(scripts
                .iter()
                .find(|script| script.id() == id)
                .cloned()
                .expect("only enabled scripts are fetched"))
        });
        Arc::new(mock_scripts)
    }

    #[tokio::test]
    async fn parse_content_derives_metadata_from_the_given_content() {
        let pdf = transcript::tests::build_pdf("hello world");
        let client = ParserClient::new(no_op_scripts());

        let metadata = client.parse_content(&pdf, "report.pdf").await.unwrap();

        assert_eq!(metadata.name, "report");
        assert_eq!(metadata.original_name, "report.pdf");
        assert_eq!(metadata.content_type, PDF_CONTENT_TYPE);
        assert_eq!(metadata.size, pdf.len() as u64);
        assert!(metadata.transcript.contains("hello world"));
    }

    #[tokio::test]
    async fn run_scripts_runs_every_enabled_script() {
        let scripts = vec![
            FakeScript::new().with_script("return 1 + 1").build(),
            FakeScript::new().with_script("return 2 + 2").build(),
        ];
        let client = ParserClient::new(enabled_scripts(scripts));
        let document = FakeDocument::new().build();

        client.run_scripts(&document).await.unwrap();
    }

    #[tokio::test]
    async fn run_scripts_fails_if_a_script_errors() {
        let scripts = vec![
            FakeScript::new()
                .with_script("this is not valid lua")
                .build(),
        ];
        let client = ParserClient::new(enabled_scripts(scripts));
        let document = FakeDocument::new().build();

        client.run_scripts(&document).await.unwrap_err();
    }

    #[tokio::test]
    async fn run_scripts_fails_instead_of_hanging_on_a_script_that_never_terminates() {
        let scripts = vec![FakeScript::new().with_script("while true do end").build()];
        let client = ParserClient::new(enabled_scripts(scripts));
        let document = FakeDocument::new().build();

        client.run_scripts(&document).await.unwrap_err();
    }

    #[tokio::test]
    async fn run_scripts_returns_the_metadata_a_script_rename_produces() {
        let scripts = vec![
            FakeScript::new()
                .with_script(r#"document.metadata.name = "renamed.pdf""#)
                .build(),
        ];
        let client = ParserClient::new(enabled_scripts(scripts));
        let document = FakeDocument::new().build();

        let metadata = client.run_scripts(&document).await.unwrap();

        assert_eq!(
            metadata,
            Metadata {
                name: "renamed.pdf".to_string(),
                ..document.metadata().clone()
            }
        );
    }

    #[tokio::test]
    async fn run_scripts_keeps_the_original_metadata_when_no_script_changes_it() {
        let scripts = vec![FakeScript::new().with_script("return 1 + 1").build()];
        let client = ParserClient::new(enabled_scripts(scripts));
        let document = FakeDocument::new().build();

        let metadata = client.run_scripts(&document).await.unwrap();

        assert_eq!(metadata, *document.metadata());
    }

    #[tokio::test]
    async fn run_scripts_fails_if_an_enabled_script_cant_be_fetched() {
        let script = FakeScript::new().build();
        let mut mock_scripts = MockScriptsService::new();
        let metadata = vec![ScriptMetadata::from(script)];
        mock_scripts
            .expect_list_user_scripts()
            .times(1)
            .return_once(move || Ok(metadata));
        mock_scripts
            .expect_fetch_script()
            .times(1)
            .returning(|id| Err(crate::ErrorKind::ScriptNotInstalled(id).into()));

        let client = ParserClient::new(Arc::new(mock_scripts));
        let document = FakeDocument::new().build();

        client.run_scripts(&document).await.unwrap_err();
    }

    #[tokio::test]
    async fn run_scripts_chains_each_scripts_changes_into_the_next() {
        let scripts = vec![
            FakeScript::new()
                .with_script(r#"document.metadata.name = "first.pdf""#)
                .build(),
            FakeScript::new()
                .with_script(r#"document.metadata.name = document.metadata.name .. "-second""#)
                .build(),
        ];
        let client = ParserClient::new(enabled_scripts(scripts));
        let document = FakeDocument::new().build();

        let metadata = client.run_scripts(&document).await.unwrap();

        assert_eq!(metadata.name, "first.pdf-second");
    }
}
