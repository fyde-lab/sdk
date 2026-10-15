use std::str::FromStr as _;
use std::sync::Arc;

use mlua::{Function, Lua, Table, Value};
use tokio::runtime::Handle;

use crate::ErrorCode;
use crate::domains::documents::parser::attributes_from_table;
use crate::domains::documents::{
    Category, DocumentType, Service as DocumentsService, Subcategory, UploadRequest,
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
/// `category`, `subcategory`, and `attributes` —
/// `{ price = { type = "float", value = 32.3 } }`, the same shape as a
/// classification script's `document.metadata.attributes`) instead
/// of leaving them to be derived later by a classification script. A
/// document whose content is already saved locally is silently skipped,
/// not an error, so re-running a scraper doesn't fail on what it already
/// fetched.
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

            // A scraper re-run typically finds documents it already saved:
            // that's not a failure, just nothing to do.
            let id = match runtime.block_on(documents.upload(request)) {
                Ok(id) => id,
                Err(err) if err.code() == ErrorCode::DuplicateDocument => {
                    tracing::debug!(
                        target: "fyde::scrapers",
                        scraper = %scraper_name,
                        name,
                        "document already saved, skipping: {err}"
                    );
                    return Ok(());
                }
                Err(err) => return Err(mlua::Error::external(err)),
            };

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
        request.r#type = Some(DocumentType::from_str(&r#type).map_err(mlua::Error::external)?);
    }
    if let Some(category) = fields.get::<Option<String>>("category")? {
        request.category = Some(Category::from_str(&category).map_err(mlua::Error::external)?);
    }
    if let Some(subcategory) = fields.get::<Option<String>>("subcategory")? {
        request.subcategory =
            Some(Subcategory::from_str(&subcategory).map_err(mlua::Error::external)?);
    }
    if let Some(attributes) = fields.get::<Option<Table>>("attributes")? {
        request.attributes =
            attributes_from_table(Some(attributes)).map_err(mlua::Error::external)?;
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
                    && request.r#type == Some(crate::domains::documents::DocumentType::Payslip)
                    && request.category == Some(crate::domains::documents::Category::Work)
                    && request.subcategory == Some(crate::domains::documents::Subcategory::Payslips)
                    && request.attributes
                        == std::collections::BTreeMap::from([(
                            "net_pay".to_string(),
                            crate::domains::documents::Attribute::Float(2150.5),
                        )])
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
                category = "work",
                subcategory = "payslips",
                attributes = { net_pay = { type = "float", value = 2150.5 } },
            })"#,
        )
        .exec()
        .unwrap();
    }

    #[test]
    fn save_document_does_nothing_and_succeeds_for_a_document_already_saved() {
        let mut documents = MockDocumentsService::new();
        documents
            .expect_upload()
            .times(1)
            .returning(|_| Err(crate::ErrorKind::DuplicateDocument(Uuid::now_v7()).into()));

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

        lua.load(r#"save_document("january.pdf", "the pdf content")"#)
            .exec()
            .unwrap();
    }

    #[test]
    fn save_document_propagates_any_other_upload_error() {
        let mut documents = MockDocumentsService::new();
        documents.expect_upload().times(1).returning(|_| {
            Err(crate::ErrorKind::UnsupportedDocumentExtension("txt".into()).into())
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

        let result = lua
            .load(r#"save_document("january.pdf", "the pdf content")"#)
            .exec();

        assert!(result.is_err());
    }

    #[test]
    fn save_document_rejects_an_invalid_category() {
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
            .load(r#"save_document("a.pdf", "content", {category = "not_a_category"})"#)
            .exec();

        assert!(result.is_err());
    }

    #[test]
    fn save_document_rejects_an_invalid_type() {
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
            .load(r#"save_document("a.pdf", "content", {type = "energy_invoice"})"#)
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
