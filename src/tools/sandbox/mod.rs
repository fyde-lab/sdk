use std::cell::Cell;

use mlua::chunk::{Chunk, ChunkMode};
use mlua::{HookTriggers, Lua, LuaOptions, StdLib, VmState};

use crate::{ErrorContext as _, Result};

/// How many Lua VM instructions run between two checks of a VM's
/// instruction budget (see [`Limits::max_instructions`]). Low enough that a
/// runaway loop overshoots its budget by a negligible amount, high enough
/// that the hook itself costs nothing measurable.
const HOOK_INTERVAL: u32 = 10_000;

/// Resource limits for one sandboxed Lua VM, so a script (often
/// server-supplied — see `documents::parser` and `scrapers`) can't hang or
/// crash its host by looping forever or allocating without bound.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// Upper bound on the VM's total heap, in bytes. An allocation past it
    /// fails the script with [`mlua::Error::MemoryError`] instead of
    /// growing the host process.
    pub(crate) memory_bytes: usize,
    /// Upper bound on the number of Lua VM instructions the script may
    /// execute over the VM's whole lifetime. Counted in instructions rather
    /// than wall-clock time on purpose: a scraper legitimately spends most
    /// of its time blocked inside host functions (network calls,
    /// `fyde.input.ask` waiting on the user), none of which executes a Lua
    /// instruction, while a runaway `while true do end` burns through the
    /// budget within seconds.
    pub(crate) max_instructions: u64,
}

/// Builds a sandboxed Lua VM loading only `libs`, with `limits` enforced.
///
/// `Lua::new_with` always loads the base library (`_G`) regardless of the
/// requested [`StdLib`] flags — it unconditionally calls `luaopen_base`
/// internally, with no `StdLib` flag to opt out — so `dofile`/`loadfile`
/// (direct filesystem access) and `load` (arbitrary/binary chunk loading)
/// are stripped from the globals table by hand afterwards to close that
/// gap. Scripts themselves must be loaded through [`load_source`], never
/// `lua.load` directly, so they can't smuggle in precompiled bytecode.
pub(crate) fn new_vm(libs: StdLib, limits: Limits) -> Result<Lua> {
    let lua =
        Lua::new_with(libs, LuaOptions::new()).context("failed to create sandboxed lua vm")?;

    let globals = lua.globals();
    for unsafe_global in ["dofile", "loadfile", "load"] {
        globals
            .set(unsafe_global, mlua::Value::Nil)
            .context("failed to strip an unsafe global from the sandboxed lua vm")?;
    }

    lua.set_memory_limit(limits.memory_bytes)
        .context("failed to set the sandboxed lua vm's memory limit")?;

    let executed = Cell::new(0u64);
    let max_instructions = limits.max_instructions;
    lua.set_global_hook(
        HookTriggers::new().every_nth_instruction(HOOK_INTERVAL),
        move |_, _| {
            executed.set(executed.get() + u64::from(HOOK_INTERVAL));
            if executed.get() > max_instructions {
                Err(mlua::Error::runtime(format!(
                    "script exceeded its budget of {max_instructions} Lua instructions"
                )))
            } else {
                Ok(VmState::Continue)
            }
        },
    )
    .context("failed to install the sandboxed lua vm's instruction budget")?;

    Ok(lua)
}

/// Loads `source` into `lua` as Lua *source text* only. `mlua` otherwise
/// auto-detects precompiled bytecode by its signature and runs it as-is —
/// even in safe mode — and the Lua VM does no verification of bytecode, so
/// a crafted chunk can corrupt the host's memory. Every script handed to a
/// [`new_vm`] VM must go through here.
pub(crate) fn load_source<'a>(lua: &Lua, source: &'a str) -> Chunk<'a> {
    lua.load(source).set_mode(ChunkMode::Text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            memory_bytes: 16 * 1024 * 1024,
            max_instructions: 1_000_000,
        }
    }

    #[test]
    fn new_vm_strips_filesystem_and_chunk_loading_globals() {
        let lua = new_vm(StdLib::TABLE | StdLib::STRING, limits()).unwrap();

        for global in ["dofile", "loadfile", "load", "io", "os", "require"] {
            let value: mlua::Value = lua.globals().get(global).unwrap();
            assert!(value.is_nil(), "{global} should not be reachable");
        }
    }

    #[test]
    fn new_vm_stops_a_script_that_never_terminates() {
        let lua = new_vm(StdLib::TABLE, limits()).unwrap();

        let err = load_source(&lua, "while true do end").exec().unwrap_err();

        assert!(err.to_string().contains("instruction"), "{err}");
    }

    #[test]
    fn new_vm_stops_a_script_that_allocates_without_bound() {
        let lua = new_vm(StdLib::STRING, limits()).unwrap();

        let err = load_source(&lua, r#"local s = string.rep("x", 64 * 1024 * 1024)"#)
            .exec()
            .unwrap_err();

        assert!(matches!(err, mlua::Error::MemoryError(_)), "{err}");
    }

    #[test]
    fn new_vm_still_runs_a_script_within_its_limits() {
        let lua = new_vm(StdLib::TABLE | StdLib::STRING, limits()).unwrap();

        let sum: i64 = load_source(
            &lua,
            "local n = 0 for i = 1, 1000 do n = n + i end return n",
        )
        .eval()
        .unwrap();

        assert_eq!(sum, 500_500);
    }

    #[test]
    fn load_source_never_treats_a_script_as_precompiled_bytecode() {
        let lua = new_vm(StdLib::STRING, limits()).unwrap();
        // Starts with the `\x1bLua` signature `mlua` would otherwise
        // auto-detect and run as unverified bytecode.
        let disguised = "\x1bLua\x54\x00 not really bytecode";

        assert_eq!(lua.load(disguised).mode(), ChunkMode::Binary);
        assert_eq!(load_source(&lua, disguised).mode(), ChunkMode::Text);
        assert!(load_source(&lua, disguised).exec().is_err());
    }
}
