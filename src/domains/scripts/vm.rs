use mlua::{Lua, LuaOptions, StdLib};
use uuid::Uuid;

use crate::domains::documents::{Document, Metadata};
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
pub(crate) fn sandboxed() -> Result<Lua> {
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
pub(crate) fn expose_document(lua: &Lua, document: &Document) -> Result<()> {
    let metadata_table = metadata_to_table(lua, document.metadata())?;

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
        .set("source_category", metadata.source_category())
        .context("failed to set document.metadata.source_category")?;
    metadata_table
        .set("source_sub_category", metadata.source_sub_category())
        .context("failed to set document.metadata.source_sub_category")?;
    metadata_table
        .set("subject", metadata.subject())
        .context("failed to set document.metadata.subject")?;
    metadata_table
        .set("qualification", metadata.qualification())
        .context("failed to set document.metadata.qualification")?;

    Ok(metadata_table)
}

/// Reads `document.metadata` back out of `lua` into a [`Metadata`], mirroring
/// every field [`expose_document`] wrote into it, except `original_name`,
/// `content_type`, `size`, `checksum`, and `transcript` — those describe the
/// underlying file rather than user-editable metadata, so a script can't
/// change them: they're always taken from `original` regardless of what a
/// script assigned into the table. Called once a script has finished
/// running, so the caller can compare the result against `original` to see
/// which fields, if any, the script changed.
pub(crate) fn read_metadata(lua: &Lua, original: &Metadata) -> Result<Metadata> {
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
        source_category: metadata_table
            .get("source_category")
            .context("failed to read document.metadata.source_category back")?,
        source_sub_category: metadata_table
            .get("source_sub_category")
            .context("failed to read document.metadata.source_sub_category back")?,
        subject: metadata_table
            .get("subject")
            .context("failed to read document.metadata.subject back")?,
        qualification: metadata_table
            .get("qualification")
            .context("failed to read document.metadata.qualification back")?,
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

        expose_document(&lua, &document).unwrap();

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

        expose_document(&lua, &document).unwrap();

        let metadata = read_metadata(&lua, document.metadata()).unwrap();

        assert_eq!(metadata, *document.metadata());
    }

    #[test]
    fn read_metadata_reflects_a_field_a_script_assigned() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        expose_document(&lua, &document).unwrap();
        lua.load(r#"document.metadata.name = "new-name.pdf""#)
            .exec()
            .unwrap();

        let metadata = read_metadata(&lua, document.metadata()).unwrap();

        assert_eq!(metadata.name, "new-name.pdf");
        assert_eq!(metadata.transcript, document.metadata().transcript());
    }

    #[test]
    fn read_metadata_keeps_the_latest_value_across_multiple_assignments() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        expose_document(&lua, &document).unwrap();
        lua.load(
            r#"
            document.metadata.name = "first.pdf"
            document.metadata.name = "second.pdf"
            "#,
        )
        .exec()
        .unwrap();

        let metadata = read_metadata(&lua, document.metadata()).unwrap();

        assert_eq!(metadata.name, "second.pdf");
    }

    #[test]
    fn read_metadata_ignores_script_writes_to_immutable_fields() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        expose_document(&lua, &document).unwrap();
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

        let metadata = read_metadata(&lua, document.metadata()).unwrap();

        assert_eq!(metadata.original_name, document.metadata().original_name());
        assert_eq!(metadata.content_type, document.metadata().content_type());
        assert_eq!(metadata.size, document.metadata().size());
        assert_eq!(metadata.checksum, document.metadata().checksum());
        assert_eq!(metadata.transcript, document.metadata().transcript());
    }
}
