use mlua::{Lua, Result as LuaResult, Table};

use crate::tools::sandbox;

/// Lua source of `fyde.helpers` (see `helpers.lua`'s header comment). Public
/// (re-exported as `fyde_sdk::SCRAPER_HELPERS_LUA`) so tooling that loads a
/// scraper script outside a real run, like `fyde-scripts check`, can provide
/// the same table to the script's top-level chunk.
pub const SOURCE: &str = include_str!("helpers.lua");

/// `fyde.helpers`: the generic helpers scraper scripts share (login error
/// contract, string/URL utilities, form submission, PDF downloads, browser
/// polling, a text-only PDF writer...), written in Lua rather than Rust since
/// they only compose the other `fyde.*` host functions. They look those up
/// through the `fyde` global when called, so the table can be built before
/// `fyde` is exposed.
pub(super) fn table(lua: &Lua) -> LuaResult<Table> {
    sandbox::load_source(lua, SOURCE)
        .set_name("fyde.helpers")
        .eval()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A VM whose `fyde` global holds `fyde.helpers` and the real
    /// `fyde.html`/`fyde.json` the helpers call into, then runs `stubs` (Lua
    /// code standing in for the other `fyde.*` tables a test needs).
    fn lua(stubs: &str) -> Lua {
        let lua = Lua::new();
        let fyde = lua.create_table().unwrap();
        fyde.set("helpers", table(&lua).unwrap()).unwrap();
        fyde.set("html", super::super::html::table(&lua).unwrap())
            .unwrap();
        fyde.set("json", super::super::json::table(&lua).unwrap())
            .unwrap();
        lua.globals().set("fyde", fyde).unwrap();
        lua.load(stubs).exec().unwrap();
        lua
    }

    fn eval<T: mlua::FromLuaMulti>(lua: &Lua, code: &str) -> T {
        lua.load(code).eval().unwrap()
    }

    #[test]
    fn table_loads_without_the_rest_of_fyde() {
        let lua = lua("");

        let _helpers: Table = eval(&lua, "return fyde.helpers");
    }

    #[test]
    fn fail_raises_exactly_the_code_without_a_position_prefix() {
        let lua = lua("fyde.log = { warn = function() end }");

        let message: String = eval(
            &lua,
            "local h = fyde.helpers \
             local _, err = pcall(h.fail, h.INVALID_CREDENTIALS, 'bad') return err",
        );

        assert_eq!(message, "INVALID_CREDENTIALS");
    }

    #[test]
    fn contains_ignores_case_and_a_missing_haystack() {
        let lua = lua("");

        let (found, missing): (bool, bool) = eval(
            &lua,
            r#"local h = fyde.helpers
               return h.contains("Mot de PASSE", "passe"), h.contains(nil, "x")"#,
        );

        assert!(found);
        assert!(!missing);
    }
}
