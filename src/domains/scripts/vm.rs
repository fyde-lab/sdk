use std::cell::RefCell;
use std::rc::Rc;

use mlua::{Lua, LuaOptions, StdLib};

use crate::domains::documents::Document;
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

/// Exposes `document` as a read-only-by-convention global table in `lua`,
/// scoping a script's access to the document it's being run against: its raw
/// `content` plus a `metadata` sub-table mirroring [`Metadata`]'s fields, and
/// a `set_name` method a script can call to rename the document. Nothing
/// beyond this document's own data is reachable from a script's Lua state.
///
/// `set_name` never touches `document`, and this function never clones it or
/// its [`Metadata`]: it only records the last name a script passed to
/// `set_name` in the returned cell, which the caller compares against
/// `document.metadata().name()` once the script has finished running to see
/// whether a rename actually happened.
///
/// [`Metadata`]: crate::domains::documents::Metadata
pub(super) fn expose_document(
    lua: &Lua,
    document: &Document,
) -> Result<Rc<RefCell<Option<String>>>> {
    let metadata = document.metadata();
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

    let new_name: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let set_name = {
        let new_name = Rc::clone(&new_name);
        lua.create_function(move |_, name: String| {
            *new_name.borrow_mut() = Some(name);
            Ok(())
        })
        .context("failed to create document.set_name lua function")?
    };
    document_table
        .set("set_name", set_name)
        .context("failed to set document.set_name")?;

    lua.globals()
        .set("document", document_table)
        .context("failed to expose document to the sandboxed lua vm")?;

    Ok(new_name)
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
    fn expose_document_leaves_the_pending_name_empty_when_set_name_is_never_called() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let new_name = expose_document(&lua, &document).unwrap();

        assert_eq!(*new_name.borrow(), None);
    }

    #[test]
    fn expose_document_records_the_name_passed_to_set_name() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let new_name = expose_document(&lua, &document).unwrap();
        lua.load(r#"document.set_name("new-name.pdf")"#)
            .exec()
            .unwrap();

        assert_eq!(new_name.borrow().as_deref(), Some("new-name.pdf"));
    }

    #[test]
    fn expose_document_keeps_the_latest_name_across_multiple_set_name_calls() {
        let lua = sandboxed().unwrap();
        let document = FakeDocument::new().build();

        let new_name = expose_document(&lua, &document).unwrap();
        lua.load(
            r#"
            document.set_name("first.pdf")
            document.set_name("second.pdf")
            "#,
        )
        .exec()
        .unwrap();

        assert_eq!(new_name.borrow().as_deref(), Some("second.pdf"));
    }
}
