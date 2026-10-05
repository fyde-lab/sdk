use std::str::FromStr as _;
use std::sync::Arc;

use mlua::{Function, Lua, Table, Value};
use tokio::runtime::Handle;

use crate::domains::documents::{
    Purpose, Service as DocumentsService, SourceCategory, SourceSubCategory, UploadRequest,
};

/// `fyde.save_document(name, content, [fields])`: uploads `content` (raw
/// bytes, as returned by `fyde.http.download`'s `body`) directly through the
/// SDK's own [`DocumentsService::upload`] — unlike `demo-rust-fyde`, which
/// just writes to a local `--out-dir`, a document scraped here goes through
/// the same encrypted-upload path as a user-provided file, so it ends up
/// synced like any other document. `name` only needs to end in `.pdf` (the
/// only extension `upload` currently accepts); it's otherwise just a hint,
/// not the uploaded document's real identity (that's the id `upload`
/// returns). The optional `fields` table lets a script set any of
/// [`UploadRequest`]'s other metadata overrides up front (`name`, `type`,
/// `source_category`, `source_sub_category`, `subjects`, `purpose`) instead
/// of leaving them to be derived later by a classification script.
pub(super) fn save_document_fn(
    lua: &Lua,
    documents: Arc<dyn DocumentsService>,
    scraper_name: &str,
    runtime: Handle,
) -> mlua::Result<Function> {
    let scraper_name = scraper_name.to_string();

    lua.create_function(
        move |_, (name, content, fields): (String, Value, Option<Table>)| {
            let bytes = match &content {
                Value::String(s) => s.as_bytes(),
                other => {
                    return Err(mlua::Error::external(format!(
                        "fyde.save_document expected a string of bytes, got {}",
                        other.type_name()
                    )));
                }
            };

            let leaf = name.rsplit(['/', '\\']).next().unwrap_or(&name);
            let mut request = UploadRequest::from_raw(sanitize_path_segment(leaf), bytes.to_vec());
            if let Some(fields) = fields {
                apply_upload_fields(&mut request, &fields)?;
            }

            let id = runtime
                .block_on(documents.upload(request))
                .map_err(mlua::Error::external)?;

            tracing::debug!(
                target: "fyde::scrapers",
                scraper = %scraper_name,
                document = %id,
                name,
                "document uploaded"
            );

            Ok(())
        },
    )
}

/// Applies the optional metadata overrides a script passed as
/// `fyde.save_document`'s third argument onto `request`. Any key not
/// present in `fields` leaves the corresponding `request` field untouched
/// (still `None`, as set by [`UploadRequest::from_raw`]).
fn apply_upload_fields(request: &mut UploadRequest, fields: &Table) -> mlua::Result<()> {
    if let Some(name) = fields.get::<Option<String>>("name")? {
        request.name = Some(name);
    }
    if let Some(r#type) = fields.get::<Option<String>>("type")? {
        request.r#type = Some(r#type);
    }
    if let Some(source_category) = fields.get::<Option<String>>("source_category")? {
        request.source_category =
            Some(SourceCategory::from_str(&source_category).map_err(mlua::Error::external)?);
    }
    if let Some(source_sub_category) = fields.get::<Option<String>>("source_sub_category")? {
        request.source_sub_category =
            Some(SourceSubCategory::from_str(&source_sub_category).map_err(mlua::Error::external)?);
    }
    if let Some(subjects) = fields.get::<Option<Vec<String>>>("subjects")? {
        request.subjects = Some(subjects);
    }
    if let Some(purpose) = fields.get::<Option<String>>("purpose")? {
        request.purpose = Some(Purpose::from_str(&purpose).map_err(mlua::Error::external)?);
    }

    Ok(())
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
    use uuid::Uuid;

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
    fn save_document_uploads_the_raw_content_directly() {
        let id = Uuid::now_v7();
        let mut documents = MockDocumentsService::new();
        documents
            .expect_upload()
            .withf(|request| match &request.source {
                crate::domains::documents::UploadSource::Raw { name, content } => {
                    name == "january.pdf" && content == b"the pdf content"
                }
                crate::domains::documents::UploadSource::Path(_) => false,
            })
            .times(1)
            .returning(move |_| Ok(id));

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
    fn save_document_applies_optional_fields_onto_the_upload_request() {
        let id = Uuid::now_v7();
        let mut documents = MockDocumentsService::new();
        documents
            .expect_upload()
            .withf(|request| {
                request.name.as_deref() == Some("January payslip")
                    && request.r#type.as_deref() == Some("payslip")
                    && request.source_category
                        == Some(crate::domains::documents::SourceCategory::Employer)
                    && request.source_sub_category
                        == Some(crate::domains::documents::SourceSubCategory::Family)
                    && request.subjects.as_deref() == Some(["Alice".to_string()].as_slice())
                    && request.purpose == Some(crate::domains::documents::Purpose::Employment)
            })
            .times(1)
            .returning(move |_| Ok(id));

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

        lua.load(
            r#"save_document("january.pdf", "the pdf content", {
                name = "January payslip",
                type = "payslip",
                source_category = "employer",
                source_sub_category = "family",
                subjects = {"Alice"},
                purpose = "employment",
            })"#,
        )
        .exec()
        .unwrap();
    }

    #[test]
    fn save_document_rejects_an_invalid_source_category() {
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

        let result = lua
            .load(r#"save_document("a.pdf", "content", {source_category = "not_a_category"})"#)
            .exec();

        assert!(result.is_err());
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
