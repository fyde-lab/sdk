use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Lua, LuaOptions, StdLib};
use pdf_oxide::PdfDocument;
use pdf_oxide::converters::ConversionOptions;
use uuid::Uuid;

use crate::domains::documents::{Document, Metadata, Purpose, SourceCategory, SourceSubCategory};
use crate::{ErrorContext as _, Result};

/// Builds a fully sandboxed Lua VM: no `io`, `os`, `package` (so no
/// `require`), `debug`, or FFI libraries are loaded, only the side-effect
/// free `table`/`string`/`math` subset — so Lua code run in it has no path
/// to the filesystem, OS environment, subprocesses, dynamic library loading,
/// or the network.
///
/// `Lua::new_with` always loads the base library (`_G`) regardless of the
/// requested [`StdLib`] flags — it unconditionally calls `luaopen_base`
/// internally, with no `StdLib` flag to opt out — so `dofile`/`loadfile`
/// (direct filesystem access) and `load` (arbitrary/binary chunk loading)
/// are stripped from the globals table by hand afterwards to close that
/// gap.
pub(super) fn sandboxed() -> Result<Lua> {
    let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH;
    let lua =
        Lua::new_with(libs, LuaOptions::new()).context("failed to create sandboxed lua vm")?;

    let globals = lua.globals();
    for unsafe_global in ["dofile", "loadfile", "load"] {
        globals
            .set(unsafe_global, mlua::Value::Nil)
            .context("failed to strip an unsafe global from the sandboxed lua vm")?;
    }

    Ok(lua)
}

/// Exposes `document` as a global table in `lua`, scoping a script's access
/// to the document it's being run against: its raw `content` plus a
/// `metadata` sub-table mirroring every [`Metadata`] field. A script mutates
/// the document by assigning directly into `document.metadata` (e.g.
/// `document.metadata.name = "new-name.pdf"`); [`read_metadata`] reads the
/// table back out once the script has finished running so the caller can see
/// which fields, if any, changed. Nothing beyond this document's own data is
/// reachable from a script's Lua state.
///
/// Returns a shared handle to `document`'s metadata, initialized to a clone
/// of `document.metadata()`: [`expose_set_source`] writes straight into it
/// (in addition to `document.metadata`'s table copy) so its validated
/// `source_category`/`source_sub_category` are available to Rust code
/// without waiting for [`read_metadata`] to re-parse them back out of Lua.
pub(super) fn expose_document(lua: &Lua, document: &Document) -> Result<Rc<RefCell<Metadata>>> {
    let metadata = Rc::new(RefCell::new(document.metadata().clone()));
    let metadata_table = metadata_to_table(lua, &metadata.borrow())?;

    let document_table = lua
        .create_table()
        .context("failed to create the document lua table")?;
    document_table
        .set("id", document.id().to_string())
        .context("failed to set document.id")?;
    let content = lua
        .create_string(document.content())
        .context("failed to create document.content lua string")?;
    document_table
        .set("content", content)
        .context("failed to set document.content")?;
    document_table
        .set("metadata", metadata_table)
        .context("failed to set document.metadata")?;

    lua.globals()
        .set("document", document_table)
        .context("failed to expose document to the sandboxed lua vm")?;

    Ok(metadata)
}

/// Exposes `pdf_as_markdown()` and `pdf_as_html()` as global functions in
/// `lua`, letting a script re-render `content` (the document's own PDF
/// bytes) as Markdown or HTML on demand — richer structure than the flat
/// `document.metadata.transcript` string, for scripts whose classification
/// needs headings, tables, or other layout. Each call re-parses `content`
/// from scratch rather than caching a decoded [`PdfDocument`] across calls,
/// since a script calls these at most a handful of times per run and
/// `PdfDocument` isn't `mlua::UserData`.
pub(super) fn expose_pdf_conversions(lua: &Lua, content: &[u8]) -> Result<()> {
    let markdown_content = content.to_vec();
    let pdf_as_markdown = lua
        .create_function(move |_, ()| {
            let pdf =
                PdfDocument::from_bytes(markdown_content.clone()).map_err(mlua::Error::external)?;
            pdf.to_markdown_all(&ConversionOptions::default())
                .map_err(mlua::Error::external)
        })
        .context("failed to create the pdf_as_markdown lua function")?;
    lua.globals()
        .set("pdf_as_markdown", pdf_as_markdown)
        .context("failed to expose pdf_as_markdown to the sandboxed lua vm")?;

    let html_content = content.to_vec();
    let pdf_as_html = lua
        .create_function(move |_, ()| {
            let pdf =
                PdfDocument::from_bytes(html_content.clone()).map_err(mlua::Error::external)?;
            pdf.to_html_all(&ConversionOptions::default())
                .map_err(mlua::Error::external)
        })
        .context("failed to create the pdf_as_html lua function")?;
    lua.globals()
        .set("pdf_as_html", pdf_as_html)
        .context("failed to expose pdf_as_html to the sandboxed lua vm")?;

    Ok(())
}

/// Exposes `set_source(category, sub_category)` as a global function in
/// `lua`, letting a script set `document.metadata.source_category` and
/// `document.metadata.source_sub_category` with immediate validation: an
/// unknown category or sub-category name raises a Lua error right away,
/// rather than leaving an invalid value sitting in `document.metadata` to
/// only be caught later by [`read_metadata`], after the script has already
/// finished running. `sub_category` is optional — omitting it (or passing
/// `nil`) clears `document.metadata.source_sub_category`.
///
/// `metadata` is the same handle [`expose_document`] returned for this
/// document: a call writes the parsed category/sub-category into it
/// directly, alongside the `document.metadata` table, so the original
/// [`Metadata`] is updated immediately rather than only once
/// [`read_metadata`] re-parses the table at the end of the script.
pub(super) fn expose_set_source(lua: &Lua, metadata: Rc<RefCell<Metadata>>) -> Result<()> {
    let set_source = lua
        .create_function(
            move |lua, (category, sub_category): (String, Option<String>)| {
                let category = category
                    .parse::<SourceCategory>()
                    .map_err(mlua::Error::external)?;
                let sub_category = sub_category
                    .map(|value| value.parse::<SourceSubCategory>())
                    .transpose()
                    .map_err(mlua::Error::external)?;

                {
                    let mut metadata = metadata.borrow_mut();
                    metadata.source_category = Some(category);
                    metadata.source_sub_category = sub_category;
                }

                let document_table: mlua::Table = lua.globals().get("document")?;
                let metadata_table: mlua::Table = document_table.get("metadata")?;
                metadata_table.set("source_category", category.as_str())?;
                metadata_table.set(
                    "source_sub_category",
                    sub_category.map(|sub_category| sub_category.as_str()),
                )?;

                Ok(())
            },
        )
        .context("failed to create the set_source lua function")?;
    lua.globals()
        .set("set_source", set_source)
        .context("failed to expose set_source to the sandboxed lua vm")?;

    Ok(())
}

/// Exposes `set_purpose(purpose)` as a global function in `lua`, letting a
/// script set `document.metadata.purpose` with immediate validation: an
/// unknown purpose name raises a Lua error right away, rather than leaving
/// an invalid value sitting in `document.metadata` to only be caught later
/// by [`read_metadata`], after the script has already finished running.
///
/// `metadata` is the same handle [`expose_document`] returned for this
/// document: a call writes the parsed purpose into it directly, alongside
/// the `document.metadata` table, so the original [`Metadata`] is updated
/// immediately rather than only once [`read_metadata`] re-parses the table
/// at the end of the script.
pub(super) fn expose_set_purpose(lua: &Lua, metadata: Rc<RefCell<Metadata>>) -> Result<()> {
    let set_purpose = lua
        .create_function(move |lua, purpose: String| {
            let purpose = purpose.parse::<Purpose>().map_err(mlua::Error::external)?;

            metadata.borrow_mut().purpose = Some(purpose);

            let document_table: mlua::Table = lua.globals().get("document")?;
            let metadata_table: mlua::Table = document_table.get("metadata")?;
            metadata_table.set("purpose", purpose.as_str())?;

            Ok(())
        })
        .context("failed to create the set_purpose lua function")?;
    lua.globals()
        .set("set_purpose", set_purpose)
        .context("failed to expose set_purpose to the sandboxed lua vm")?;

    Ok(())
}

fn metadata_to_table(lua: &Lua, metadata: &Metadata) -> Result<mlua::Table> {
    let metadata_table = lua
        .create_table()
        .context("failed to create the document.metadata lua table")?;
    metadata_table
        .set("id", metadata.id().to_string())
        .context("failed to set document.metadata.id")?;
    metadata_table
        .set("name", metadata.name())
        .context("failed to set document.metadata.name")?;
    metadata_table
        .set("original_name", metadata.original_name())
        .context("failed to set document.metadata.original_name")?;
    metadata_table
        .set("content_type", metadata.content_type())
        .context("failed to set document.metadata.content_type")?;
    metadata_table
        .set("created_at", metadata.created_at())
        .context("failed to set document.metadata.created_at")?;
    metadata_table
        .set("size", metadata.size())
        .context("failed to set document.metadata.size")?;
    metadata_table
        .set("checksum", metadata.checksum())
        .context("failed to set document.metadata.checksum")?;
    metadata_table
        .set("transcript", metadata.transcript())
        .context("failed to set document.metadata.transcript")?;
    metadata_table
        .set("type", metadata.r#type())
        .context("failed to set document.metadata.type")?;
    metadata_table
        .set(
            "source_category",
            metadata
                .source_category()
                .map_or("", |category| category.as_str()),
        )
        .context("failed to set document.metadata.source_category")?;
    metadata_table
        .set(
            "source_sub_category",
            metadata
                .source_sub_category()
                .map(|category| category.as_str()),
        )
        .context("failed to set document.metadata.source_sub_category")?;
    metadata_table
        .set("subject", metadata.subject())
        .context("failed to set document.metadata.subject")?;
    metadata_table
        .set("qualification", metadata.qualification())
        .context("failed to set document.metadata.qualification")?;
    metadata_table
        .set(
            "purpose",
            metadata.purpose().map_or("", |purpose| purpose.as_str()),
        )
        .context("failed to set document.metadata.purpose")?;

    Ok(metadata_table)
}

/// Reads `document.metadata` back out of `lua` into a [`Metadata`], mirroring
/// every field [`expose_document`] wrote into it, except `original_name`,
/// `content_type`, `size`, `checksum`, and `transcript` — those describe the
/// underlying file rather than user-editable metadata, so a script can't
/// change them: they're always taken from `original` regardless of what a
/// script assigned into the table. `source_category`/`source_sub_category`
/// and `purpose` are likewise taken from `source` (the
/// [`expose_document`]-returned handle [`expose_set_source`]/
/// [`expose_set_purpose`] write into) rather than re-parsed out of the
/// table, since that handle is already validated and is the only way those
/// fields can change. Called once a script has finished running, so the
/// caller can compare the result against `original` to see which fields, if
/// any, the script changed.
pub(super) fn read_metadata(lua: &Lua, original: &Metadata, source: &Metadata) -> Result<Metadata> {
    let document_table: mlua::Table = lua
        .globals()
        .get("document")
        .context("failed to read the document lua global back")?;
    let metadata_table: mlua::Table = document_table
        .get("metadata")
        .context("failed to read document.metadata back")?;

    let id: String = metadata_table
        .get("id")
        .context("failed to read document.metadata.id back")?;
    let id = Uuid::parse_str(&id).context("script left document.metadata.id as an invalid uuid")?;

    Ok(Metadata {
        id,
        name: metadata_table
            .get("name")
            .context("failed to read document.metadata.name back")?,
        original_name: original.original_name().to_string(),
        content_type: original.content_type().to_string(),
        created_at: metadata_table
            .get("created_at")
            .context("failed to read document.metadata.created_at back")?,
        size: original.size(),
        checksum: original.checksum().to_string(),
        transcript: original.transcript().to_string(),
        r#type: metadata_table
            .get("type")
            .context("failed to read document.metadata.type back")?,
        source_category: source.source_category(),
        source_sub_category: source.source_sub_category(),
        subject: metadata_table
            .get("subject")
            .context("failed to read document.metadata.subject back")?,
        qualification: metadata_table
            .get("qualification")
            .context("failed to read document.metadata.qualification back")?,
        purpose: source.purpose(),
    })
}

#[cfg(test)]
mod tests {
    use mlua::Value;

    use super::*;
    use crate::domains::documents::FakeDocument;

    #[test]
    fn sandboxed_vm_has_no_filesystem_or_process_access() {
        let lua = sandboxed().unwrap();

        for global in [
            "os", "io", "require", "dofile", "loadfile", "load", "package",
        ] {
            assert!(
                matches!(lua.globals().get::<Value>(global).unwrap(), Value::Nil),
                "expected global {global:?} to be unavailable in the sandboxed vm"
            );
        }
    }

    #[test]
    fn sandboxed_vm_can_still_run_safe_lua() {
        let lua = sandboxed().unwrap();

        let sum: i64 = lua.load("return 1 + 41").eval().unwrap();

        assert_eq!(sum, 42);
    }

    #[test]
    fn expose_document_sets_content_and_metadata_globals() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let _metadata = expose_document(&lua, &document).unwrap();

        let content: mlua::LuaString = lua.load("return document.content").eval().unwrap();
        assert_eq!(content.as_bytes().to_vec(), document.content());

        let name: String = lua.load("return document.metadata.name").eval().unwrap();
        assert_eq!(name, document.metadata().name());

        let transcript: String = lua
            .load("return document.metadata.transcript")
            .eval()
            .unwrap();
        assert_eq!(transcript, document.metadata().transcript());
    }

    #[test]
    fn read_metadata_matches_the_original_when_a_script_makes_no_changes() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();

        let metadata = read_metadata(&lua, document.metadata(), &source_metadata.borrow()).unwrap();

        assert_eq!(metadata, *document.metadata());
    }

    #[test]
    fn read_metadata_reflects_a_field_a_script_assigned() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        lua.load(r#"document.metadata.name = "new-name.pdf""#)
            .exec()
            .unwrap();

        let metadata = read_metadata(&lua, document.metadata(), &source_metadata.borrow()).unwrap();

        assert_eq!(metadata.name, "new-name.pdf");
        assert_eq!(metadata.transcript, document.metadata().transcript());
    }

    #[test]
    fn read_metadata_keeps_the_latest_value_across_multiple_assignments() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        lua.load(
            r#"
            document.metadata.name = "first.pdf"
            document.metadata.name = "second.pdf"
            "#,
        )
        .exec()
        .unwrap();

        let metadata = read_metadata(&lua, document.metadata(), &source_metadata.borrow()).unwrap();

        assert_eq!(metadata.name, "second.pdf");
    }

    #[test]
    fn pdf_as_markdown_renders_the_documents_pdf_content_as_markdown() {
        let lua = sandboxed().unwrap();
        let pdf = crate::domains::documents::parser::transcript::tests::build_pdf("Hello World!");

        expose_pdf_conversions(&lua, &pdf).unwrap();

        let markdown: String = lua.load("return pdf_as_markdown()").eval().unwrap();
        assert!(markdown.contains("Hello World!"));
    }

    #[test]
    fn pdf_as_html_renders_the_documents_pdf_content_as_html() {
        let lua = sandboxed().unwrap();
        let pdf = crate::domains::documents::parser::transcript::tests::build_pdf("Hello World!");

        expose_pdf_conversions(&lua, &pdf).unwrap();

        let html: String = lua.load("return pdf_as_html()").eval().unwrap();
        assert!(html.contains("Hello World!"));
    }

    #[test]
    fn set_source_sets_the_category_and_sub_category() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        expose_set_source(&lua, source_metadata.clone()).unwrap();
        lua.load(r#"set_source("bank", "tax")"#).exec().unwrap();

        assert_eq!(
            source_metadata.borrow().source_category,
            Some(SourceCategory::Bank)
        );
        assert_eq!(
            source_metadata.borrow().source_sub_category,
            Some(SourceSubCategory::Tax)
        );

        let metadata = read_metadata(&lua, document.metadata(), &source_metadata.borrow()).unwrap();

        assert_eq!(metadata.source_category, Some(SourceCategory::Bank));
        assert_eq!(metadata.source_sub_category, Some(SourceSubCategory::Tax));
    }

    #[test]
    fn set_source_accepts_a_nil_sub_category() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        expose_set_source(&lua, source_metadata.clone()).unwrap();
        lua.load(r#"set_source("bank")"#).exec().unwrap();

        let metadata = read_metadata(&lua, document.metadata(), &source_metadata.borrow()).unwrap();

        assert_eq!(metadata.source_category, Some(SourceCategory::Bank));
        assert_eq!(metadata.source_sub_category, None);
    }

    #[test]
    fn set_source_errors_on_an_unknown_category() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        expose_set_source(&lua, source_metadata).unwrap();

        let result = lua.load(r#"set_source("not-a-category")"#).exec();

        assert!(result.is_err());
    }

    #[test]
    fn set_source_errors_on_an_unknown_sub_category() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        expose_set_source(&lua, source_metadata).unwrap();

        let result = lua
            .load(r#"set_source("bank", "not-a-sub-category")"#)
            .exec();

        assert!(result.is_err());
    }

    #[test]
    fn set_purpose_sets_the_purpose() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        expose_set_purpose(&lua, source_metadata.clone()).unwrap();
        lua.load(r#"set_purpose("invoice")"#).exec().unwrap();

        assert_eq!(source_metadata.borrow().purpose, Some(Purpose::Invoice));

        let metadata = read_metadata(&lua, document.metadata(), &source_metadata.borrow()).unwrap();

        assert_eq!(metadata.purpose, Some(Purpose::Invoice));
    }

    #[test]
    fn set_purpose_errors_on_an_unknown_purpose() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        expose_set_purpose(&lua, source_metadata).unwrap();

        let result = lua.load(r#"set_purpose("not-a-purpose")"#).exec();

        assert!(result.is_err());
    }

    #[test]
    fn read_metadata_ignores_script_writes_to_immutable_fields() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        lua.load(
            r#"
            document.metadata.original_name = "hacked.pdf"
            document.metadata.content_type = "application/x-hacked"
            document.metadata.size = 999999
            document.metadata.checksum = "hacked"
            document.metadata.transcript = "hacked transcript"
            "#,
        )
        .exec()
        .unwrap();

        let metadata = read_metadata(&lua, document.metadata(), &source_metadata.borrow()).unwrap();

        assert_eq!(metadata.original_name, document.metadata().original_name());
        assert_eq!(metadata.content_type, document.metadata().content_type());
        assert_eq!(metadata.size, document.metadata().size());
        assert_eq!(metadata.checksum, document.metadata().checksum());
        assert_eq!(metadata.transcript, document.metadata().transcript());
    }
}
