use mlua::{Lua, LuaOptions, StdLib};

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

#[cfg(test)]
mod tests {
    use mlua::Value;

    use super::*;

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
}
