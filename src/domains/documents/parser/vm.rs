use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use mlua::{Lua, StdLib};
use pdf_oxide::PdfDocument;
use pdf_oxide::converters::ConversionOptions;
use uuid::Uuid;

use crate::domains::documents::{
    Attribute, Category, Document, DocumentType, Metadata, Subcategory,
};
use crate::{ErrorContext as _, ErrorKind, Result};

/// Upper bound on the Lua instructions one classification script may run
/// against one document (see [`crate::tools::sandbox::Limits`]) — far more than a
/// script matching on a transcript ever needs, so only a runaway loop hits it.
const MAX_INSTRUCTIONS: u64 = 200_000_000;

/// Heap a classification script gets on top of what the document it runs
/// against needs (see [`sandboxed`]).
const BASE_MEMORY_BYTES: usize = 64 * 1024 * 1024;

/// Builds a fully sandboxed Lua VM: no `io`, `os`, `package` (so no
/// `require`), `debug`, or FFI libraries are loaded, only the side-effect
/// free `table`/`string`/`math` subset — so Lua code run in it has no path
/// to the filesystem, OS environment, subprocesses, dynamic library loading,
/// or the network. See [`crate::tools::sandbox::new_vm`] for the globals it strips
/// on top of that and the limits it enforces: these scripts come from the
/// server (any user's public script a user enabled), so one that loops
/// forever or allocates without bound must fail on its own rather than
/// hang or crash the app. The memory limit scales with `content_len`
/// (the document's size), since the document's content and its
/// `pdf_as_markdown`/`pdf_as_html` conversions all live on the VM's heap.
pub(super) fn sandboxed(content_len: usize) -> Result<Lua> {
    crate::tools::sandbox::new_vm(
        StdLib::TABLE | StdLib::STRING | StdLib::MATH,
        crate::tools::sandbox::Limits {
            memory_bytes: BASE_MEMORY_BYTES.saturating_add(content_len.saturating_mul(4)),
            max_instructions: MAX_INSTRUCTIONS,
        },
    )
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
/// of `document.metadata()`: [`expose_set_category`] writes straight into it
/// (in addition to `document.metadata`'s table copy) so its validated
/// `category`/`subcategory` are available to Rust code
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

/// Exposes `set_category(category, subcategory)` as a global function in
/// `lua`, letting a script set `document.metadata.category` and
/// `document.metadata.subcategory` with immediate validation: an
/// unknown category or subcategory name, or a subcategory belonging to
/// another category, raises a Lua error right away,
/// rather than leaving an invalid value sitting in `document.metadata` to
/// only be caught later by [`read_metadata`], after the script has already
/// finished running. `subcategory` is optional — omitting it (or passing
/// `nil`) clears `document.metadata.subcategory`.
///
/// `metadata` is the same handle [`expose_document`] returned for this
/// document: a call writes the parsed category/subcategory into it
/// directly, alongside the `document.metadata` table, so the original
/// [`Metadata`] is updated immediately rather than only once
/// [`read_metadata`] re-parses the table at the end of the script.
pub(super) fn expose_set_category(lua: &Lua, metadata: Rc<RefCell<Metadata>>) -> Result<()> {
    let set_category = lua
        .create_function(
            move |lua, (category, subcategory): (String, Option<String>)| {
                let category = category
                    .parse::<Category>()
                    .map_err(mlua::Error::external)?;
                let subcategory = subcategory
                    .map(|value| value.parse::<Subcategory>())
                    .transpose()
                    .map_err(mlua::Error::external)?;
                super::super::service::check_categories(Some(category), subcategory)
                    .map_err(mlua::Error::external)?;

                {
                    let mut metadata = metadata.borrow_mut();
                    metadata.category = Some(category);
                    metadata.subcategory = subcategory;
                }

                let document_table: mlua::Table = lua.globals().get("document")?;
                let metadata_table: mlua::Table = document_table.get("metadata")?;
                metadata_table.set("category", category.as_str())?;
                metadata_table.set(
                    "subcategory",
                    subcategory.map(|subcategory| subcategory.as_str()),
                )?;

                Ok(())
            },
        )
        .context("failed to create the set_category lua function")?;
    lua.globals()
        .set("set_category", set_category)
        .context("failed to expose set_category to the sandboxed lua vm")?;

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
        .set(
            "type",
            metadata.r#type().map_or("", |r#type| r#type.as_str()),
        )
        .context("failed to set document.metadata.type")?;
    metadata_table
        .set(
            "category",
            metadata.category().map_or("", |category| category.as_str()),
        )
        .context("failed to set document.metadata.category")?;
    metadata_table
        .set(
            "subcategory",
            metadata.subcategory().map(|category| category.as_str()),
        )
        .context("failed to set document.metadata.subcategory")?;
    metadata_table
        .set(
            "attributes",
            attributes_to_table(lua, metadata.attributes())?,
        )
        .context("failed to set document.metadata.attributes")?;

    Ok(metadata_table)
}

/// Mirrors `attributes` as a Lua table of `{ type = ..., value = ... }`
/// entries keyed by name — the same shape [`Attribute`] serializes to — so
/// a script adds one with e.g.
/// `document.metadata.attributes.price = { type = "float", value = 32.3 }`.
fn attributes_to_table(lua: &Lua, attributes: &BTreeMap<String, Attribute>) -> Result<mlua::Table> {
    let attributes_table = lua
        .create_table()
        .context("failed to create the document.metadata.attributes lua table")?;
    for (name, attribute) in attributes {
        let attribute_table = lua
            .create_table()
            .with_context(|| format!("failed to create the lua table for attribute {name:?}"))?;
        attribute_table
            .set("type", attribute.type_name())
            .with_context(|| format!("failed to set the type of attribute {name:?}"))?;
        let value = match attribute {
            Attribute::String(value) => attribute_table.set("value", value.as_str()),
            Attribute::Integer(value) => attribute_table.set("value", *value),
            Attribute::Float(value) => attribute_table.set("value", *value),
            Attribute::Boolean(value) => attribute_table.set("value", *value),
        };
        value.with_context(|| format!("failed to set the value of attribute {name:?}"))?;
        attributes_table
            .set(name.as_str(), attribute_table)
            .with_context(|| format!("failed to set attribute {name:?}"))?;
    }

    Ok(attributes_table)
}

/// Reads `document.metadata.attributes` back out of the table
/// [`attributes_to_table`] wrote, validating every entry a script added or
/// changed: an unknown `type`, or a `value` that doesn't fit it, fails with
/// [`ErrorKind::InvalidAttribute`]. A script that set the whole table to
/// `nil` cleared every attribute. Also parses `fyde.save_document`'s
/// `attributes` field, which takes the same shape.
pub(crate) fn attributes_from_table(
    attributes_table: Option<mlua::Table>,
) -> Result<BTreeMap<String, Attribute>> {
    let Some(attributes_table) = attributes_table else {
        return Ok(BTreeMap::new());
    };

    let mut attributes = BTreeMap::new();
    for pair in attributes_table.pairs::<String, mlua::Table>() {
        let (name, attribute_table) =
            pair.context("script left an invalid entry in document.metadata.attributes")?;
        let r#type: String = attribute_table
            .get("type")
            .with_context(|| format!("failed to read the type of attribute {name:?} back"))?;
        let value: mlua::Value = attribute_table
            .get("value")
            .with_context(|| format!("failed to read the value of attribute {name:?} back"))?;
        let attribute = match (r#type.as_str(), value) {
            ("string", mlua::Value::String(value)) => Attribute::String(
                value
                    .to_str()
                    .with_context(|| format!("attribute {name:?} has a non-UTF-8 value"))?
                    .to_string(),
            ),
            ("integer", mlua::Value::Integer(value)) => Attribute::Integer(value),
            ("float", mlua::Value::Number(value)) => Attribute::Float(value),
            ("float", mlua::Value::Integer(value)) => Attribute::Float(value as f64),
            ("boolean", mlua::Value::Boolean(value)) => Attribute::Boolean(value),
            ("string" | "integer" | "float" | "boolean", value) => {
                return Err(ErrorKind::InvalidAttribute(format!(
                    "attribute {name:?} has a {} value, not a valid {type}",
                    value.type_name()
                ))
                .into());
            }
            (other, _) => {
                return Err(ErrorKind::InvalidAttribute(format!(
                    "attribute {name:?} has unknown type {other:?}"
                ))
                .into());
            }
        };
        attributes.insert(name, attribute);
    }

    Ok(attributes)
}

/// Reads `document.metadata` back out of `lua` into a [`Metadata`], mirroring
/// every field [`expose_document`] wrote into it, except `original_name`,
/// `content_type`, `size`, `checksum`, and `transcript` — those describe the
/// underlying file rather than user-editable metadata, so a script can't
/// change them: they're always taken from `original` regardless of what a
/// script assigned into the table. `category`/`subcategory` are likewise
/// taken from `source` (the [`expose_document`]-returned handle
/// [`expose_set_category`] writes into) rather than re-parsed out of the
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

    let r#type: String = metadata_table
        .get("type")
        .context("failed to read document.metadata.type back")?;
    let r#type = if r#type.is_empty() {
        None
    } else {
        Some(
            r#type
                .parse::<DocumentType>()
                .with_context(|| format!("script set document.metadata.type to {type:?}"))?,
        )
    };

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
        r#type,
        category: source.category(),
        subcategory: source.subcategory(),
        attributes: attributes_from_table(
            metadata_table
                .get("attributes")
                .context("script left document.metadata.attributes as a non-table value")?,
        )?,
    })
}

#[cfg(test)]
mod tests {
    use mlua::Value;

    use super::*;
    use crate::domains::documents::FakeDocument;

    #[test]
    fn sandboxed_vm_has_no_filesystem_or_process_access() {
        let lua = sandboxed(0).unwrap();

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
        let lua = sandboxed(0).unwrap();

        let sum: i64 = lua.load("return 1 + 41").eval().unwrap();

        assert_eq!(sum, 42);
    }

    #[test]
    fn expose_document_sets_content_and_metadata_globals() {
        let lua = sandboxed(0).unwrap();
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
        let lua = sandboxed(0).unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();

        let metadata = read_metadata(&lua, document.metadata(), &source_metadata.borrow()).unwrap();

        assert_eq!(metadata, *document.metadata());
    }

    #[test]
    fn read_metadata_reflects_a_field_a_script_assigned() {
        let lua = sandboxed(0).unwrap();
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
        let lua = sandboxed(0).unwrap();
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
    fn read_metadata_parses_a_type_a_script_assigned() {
        let lua = sandboxed(0).unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        lua.load(r#"document.metadata.type = "payslip""#)
            .exec()
            .unwrap();

        let metadata = read_metadata(&lua, document.metadata(), &source_metadata.borrow()).unwrap();

        assert_eq!(metadata.r#type, Some(DocumentType::Payslip));
    }

    #[test]
    fn read_metadata_errors_on_an_unknown_type() {
        let lua = sandboxed(0).unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        lua.load(r#"document.metadata.type = "energy_invoice""#)
            .exec()
            .unwrap();

        let result = read_metadata(&lua, document.metadata(), &source_metadata.borrow());

        assert_eq!(
            result.unwrap_err().code(),
            crate::ErrorCode::InvalidDocumentType
        );
    }

    #[test]
    fn expose_document_exposes_existing_attributes_as_typed_tables() {
        let lua = sandboxed(0).unwrap();
        let metadata = crate::domains::documents::FakeMetadata::new()
            .with_attributes([("price".to_string(), Attribute::Float(32.3))])
            .build();
        let document = FakeDocument::new().with_metadata(metadata).build();

        expose_document(&lua, &document).unwrap();
        let (r#type, value): (String, f64) = lua
            .load(
                "return document.metadata.attributes.price.type, document.metadata.attributes.price.value",
            )
            .eval()
            .unwrap();

        assert_eq!(r#type, "float");
        assert_eq!(value, 32.3);
    }

    #[test]
    fn read_metadata_reflects_attributes_a_script_assigned() {
        let lua = sandboxed(0).unwrap();
        let metadata = crate::domains::documents::FakeMetadata::new()
            .with_attributes([("paid".to_string(), Attribute::Boolean(false))])
            .build();
        let document = FakeDocument::new().with_metadata(metadata).build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        lua.load(
            r#"
            document.metadata.attributes.price = { type = "float", value = 32.3 }
            document.metadata.attributes.quantity = { type = "integer", value = 2 }
            document.metadata.attributes.reference = { type = "string", value = "F-42" }
            document.metadata.attributes.paid.value = true
            "#,
        )
        .exec()
        .unwrap();

        let metadata = read_metadata(&lua, document.metadata(), &source_metadata.borrow()).unwrap();

        assert_eq!(
            metadata.attributes,
            BTreeMap::from([
                ("paid".to_string(), Attribute::Boolean(true)),
                ("price".to_string(), Attribute::Float(32.3)),
                ("quantity".to_string(), Attribute::Integer(2)),
                (
                    "reference".to_string(),
                    Attribute::String("F-42".to_string())
                ),
            ])
        );
    }

    #[test]
    fn read_metadata_errors_on_an_attribute_of_unknown_type() {
        let lua = sandboxed(0).unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        lua.load(r#"document.metadata.attributes.price = { type = "money", value = 32.3 }"#)
            .exec()
            .unwrap();

        let result = read_metadata(&lua, document.metadata(), &source_metadata.borrow());

        assert_eq!(
            result.unwrap_err().code(),
            crate::ErrorCode::InvalidAttribute
        );
    }

    #[test]
    fn read_metadata_errors_on_an_attribute_value_not_matching_its_type() {
        let lua = sandboxed(0).unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        lua.load(r#"document.metadata.attributes.paid = { type = "boolean", value = "yes" }"#)
            .exec()
            .unwrap();

        let result = read_metadata(&lua, document.metadata(), &source_metadata.borrow());

        assert_eq!(
            result.unwrap_err().code(),
            crate::ErrorCode::InvalidAttribute
        );
    }

    #[test]
    fn pdf_as_markdown_renders_the_documents_pdf_content_as_markdown() {
        let lua = sandboxed(0).unwrap();
        let pdf = crate::domains::documents::parser::transcript::tests::build_pdf("Hello World!");

        expose_pdf_conversions(&lua, &pdf).unwrap();

        let markdown: String = lua.load("return pdf_as_markdown()").eval().unwrap();
        assert!(markdown.contains("Hello World!"));
    }

    #[test]
    fn pdf_as_html_renders_the_documents_pdf_content_as_html() {
        let lua = sandboxed(0).unwrap();
        let pdf = crate::domains::documents::parser::transcript::tests::build_pdf("Hello World!");

        expose_pdf_conversions(&lua, &pdf).unwrap();

        let html: String = lua.load("return pdf_as_html()").eval().unwrap();
        assert!(html.contains("Hello World!"));
    }

    #[test]
    fn set_category_sets_the_category_and_subcategory() {
        let lua = sandboxed(0).unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        expose_set_category(&lua, source_metadata.clone()).unwrap();
        lua.load(r#"set_category("money", "banking")"#)
            .exec()
            .unwrap();

        assert_eq!(source_metadata.borrow().category, Some(Category::Money));
        assert_eq!(
            source_metadata.borrow().subcategory,
            Some(Subcategory::Banking)
        );

        let metadata = read_metadata(&lua, document.metadata(), &source_metadata.borrow()).unwrap();

        assert_eq!(metadata.category, Some(Category::Money));
        assert_eq!(metadata.subcategory, Some(Subcategory::Banking));
    }

    #[test]
    fn set_category_accepts_a_nil_subcategory() {
        let lua = sandboxed(0).unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        expose_set_category(&lua, source_metadata.clone()).unwrap();
        lua.load(r#"set_category("money")"#).exec().unwrap();

        let metadata = read_metadata(&lua, document.metadata(), &source_metadata.borrow()).unwrap();

        assert_eq!(metadata.category, Some(Category::Money));
        assert_eq!(metadata.subcategory, None);
    }

    #[test]
    fn set_category_errors_on_an_unknown_category() {
        let lua = sandboxed(0).unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        expose_set_category(&lua, source_metadata).unwrap();

        let result = lua.load(r#"set_category("not-a-category")"#).exec();

        assert!(result.is_err());
    }

    #[test]
    fn set_category_errors_on_an_unknown_subcategory() {
        let lua = sandboxed(0).unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        expose_set_category(&lua, source_metadata).unwrap();

        let result = lua
            .load(r#"set_category("money", "not-a-subcategory")"#)
            .exec();

        assert!(result.is_err());
    }

    #[test]
    fn set_category_errors_on_a_subcategory_of_another_category() {
        let lua = sandboxed(0).unwrap();
        let document = FakeDocument::new().build();

        let source_metadata = expose_document(&lua, &document).unwrap();
        expose_set_category(&lua, source_metadata).unwrap();

        let result = lua.load(r#"set_category("money", "energy")"#).exec();

        assert!(result.is_err());
    }

    #[test]
    fn read_metadata_ignores_script_writes_to_immutable_fields() {
        let lua = sandboxed(0).unwrap();
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
