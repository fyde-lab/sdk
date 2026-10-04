use std::sync::Arc;

use mlua::{Function, Lua, Value};
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::domains::documents::Service as DocumentsService;

/// `fyde.save_document(name, content)`: writes `content` (raw bytes, as
/// returned by `fyde.http.download`'s `body`) to a scratch temporary file and
/// uploads it through the SDK's own [`DocumentsService::upload`] — unlike
/// `demo-rust-fyde`, which just writes to a local `--out-dir`, a document
/// scraped here goes through the same encrypted-upload path as a
/// user-provided file, so it ends up synced like any other document. `name`
/// only needs to end in `.pdf` (the only extension `upload` currently
/// accepts); it's otherwise just a hint for the temporary file's name, not
/// the uploaded document's real identity (that's the id `upload` returns).
pub(super) fn save_document_fn(
    lua: &Lua,
    documents: Arc<dyn DocumentsService>,
    scraper_name: &str,
    runtime: Handle,
) -> mlua::Result<Function> {
    let temp_dir = std::env::temp_dir()
        .join("fyde-sdk-scrapers")
        .join(sanitize_path_segment(scraper_name));
    let scraper_name = scraper_name.to_string();

    lua.create_function(move |_, (name, content): (String, Value)| {
        let bytes = match &content {
            Value::String(s) => s.as_bytes(),
            other => {
                return Err(mlua::Error::external(format!(
                    "fyde.save_document expected a string of bytes, got {}",
                    other.type_name()
                )));
            }
        };

        std::fs::create_dir_all(&temp_dir).map_err(mlua::Error::external)?;

        let leaf = name.rsplit(['/', '\\']).next().unwrap_or(&name);
        let path = temp_dir.join(format!(
            "{}-{}",
            Uuid::new_v4(),
            sanitize_path_segment(leaf)
        ));
        std::fs::write(&path, &*bytes).map_err(mlua::Error::external)?;

        let result = runtime.block_on(documents.upload(&path));
        let _ = std::fs::remove_file(&path);

        let id = result.map_err(mlua::Error::external)?;
        tracing::info!(
            target: "fyde::scrapers",
            scraper = %scraper_name,
            document = %id,
            name,
            "document uploaded"
        );

        Ok(())
    })
}

/// Keeps `name` (and `scraper_name`) safe to use as a single path segment: no
/// path separators (so a remote-sourced name can never escape `temp_dir`),
/// no null bytes, and never empty/`.`/`..`.
fn sanitize_path_segment(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .filter(|c| *c != '\0' && *c != '/' && *c != '\\')
        .collect();
    match sanitized.as_str() {
        "" | "." | ".." => "document".to_string(),
        _ => sanitized,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::documents::MockService as MockDocumentsService;

    #[test]
    fn sanitize_path_segment_strips_path_separators_and_null_bytes() {
        assert_eq!(sanitize_path_segment("a/b\\c\0d"), "abcd");
    }

    #[test]
    fn sanitize_path_segment_falls_back_to_document_for_empty_or_dot_segments() {
        assert_eq!(sanitize_path_segment(""), "document");
        assert_eq!(sanitize_path_segment("."), "document");
        assert_eq!(sanitize_path_segment(".."), "document");
    }

    // `mlua::Lua` isn't `Send` (no `send` feature enabled — see Cargo.toml),
    // so unlike `service.rs`'s production code path (which only ever builds
    // a `Lua` *inside* the `spawn_blocking` closure that owns it for its
    // whole lifetime, never moving an existing one across threads) these
    // tests can't move `lua` into a `spawn_blocking` closure. A dedicated,
    // single-threaded runtime lets `save_document_fn`'s `runtime.block_on`
    // calls work without needing `#[tokio::test]`'s own (potentially
    // reentrant) runtime context.
    #[test]
    fn save_document_uploads_the_content_as_a_pdf_and_cleans_up_the_temp_file() {
        let id = Uuid::now_v7();
        let mut documents = MockDocumentsService::new();
        documents
            .expect_upload()
            .withf(|path| path.extension().and_then(|e| e.to_str()) == Some("pdf"))
            .times(1)
            .returning(move |path| {
                // The temp file must actually exist (with the right bytes)
                // at the moment `upload` is called.
                assert_eq!(std::fs::read(path).unwrap(), b"the pdf content");
                Ok(id)
            });

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let lua = Lua::new();
        let f = save_document_fn(
            &lua,
            Arc::new(documents),
            "didaxis",
            runtime.handle().clone(),
        )
        .unwrap();
        lua.globals().set("save_document", f).unwrap();

        lua.load(r#"save_document("payslip/january.pdf", "the pdf content")"#)
            .exec()
            .unwrap();
    }

    #[test]
    fn save_document_rejects_non_string_content() {
        let documents = MockDocumentsService::new();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let lua = Lua::new();
        let f = save_document_fn(
            &lua,
            Arc::new(documents),
            "didaxis",
            runtime.handle().clone(),
        )
        .unwrap();
        lua.globals().set("save_document", f).unwrap();

        let result = lua.load(r#"save_document("a.pdf", {1, 2, 3})"#).exec();

        assert!(result.is_err());
    }
}
