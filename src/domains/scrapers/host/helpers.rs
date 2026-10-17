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

    #[test]
    fn contains_any_matches_any_of_the_needles() {
        let lua = lua("");

        let (found, missing): (bool, bool) = eval(
            &lua,
            r#"local h = fyde.helpers
               return h.contains_any("Identifiant inconnu", { "mot de passe", "IDENTIFIANT" }),
                 h.contains_any("ok", { "x" })"#,
        );

        assert!(found);
        assert!(!missing);
    }

    #[test]
    fn trim_strips_surrounding_whitespace_and_a_missing_string() {
        let lua = lua("");

        let (trimmed, missing): (String, String) = eval(
            &lua,
            r#"return fyde.helpers.trim("  a b \n"), fyde.helpers.trim(nil)"#,
        );

        assert_eq!(trimmed, "a b");
        assert_eq!(missing, "");
    }

    #[test]
    fn sanitize_makes_a_safe_file_name() {
        let lua = lua("");

        let (sanitized, empty): (String, String) = eval(
            &lua,
            r#"local h = fyde.helpers return h.sanitize(' a/b:  c?.pdf '), h.sanitize("  ")"#,
        );

        assert_eq!(sanitized, "a_b_ c_.pdf");
        assert_eq!(empty, "document");
    }

    #[test]
    fn ask_code_fails_with_mfa_failed_when_nobody_answers() {
        let lua = lua("fyde.log = { warn = function() end } \
             fyde.input = { ask = function() error('no handler') end }");

        let message: String = eval(
            &lua,
            "local _, err = pcall(fyde.helpers.ask_code, 'code?') return err",
        );

        assert_eq!(message, "MFA_FAILED");
    }

    #[test]
    fn ask_code_strips_whitespace_from_the_answer() {
        let lua = lua("fyde.input = { ask = function() return ' 12 34 ' end }");

        let code: String = eval(&lua, "return fyde.helpers.ask_code('code?')");

        assert_eq!(code, "1234");
    }

    #[test]
    fn base64url_decode_decodes_a_jwt_segment() {
        let lua = lua("");

        let decoded: String = eval(
            &lua,
            r#"return fyde.helpers.base64url_decode("eyJzdWIiOiIxMjM-In0")"#,
        );

        assert_eq!(decoded, r#"{"sub":"123>"}"#);
    }

    #[test]
    fn url_encode_keeps_only_unreserved_characters() {
        let lua = lua("");

        let encoded: String = eval(&lua, r#"return fyde.helpers.url_encode("a b&c=é~")"#);

        assert_eq!(encoded, "a%20b%26c%3D%C3%A9~");
    }

    #[test]
    fn absolute_resolves_every_kind_of_href() {
        let lua = lua("");
        let absolute: mlua::Function = eval(&lua, "return fyde.helpers.absolute");
        let base = "https://example.com/a/b?x=1";

        for (href, expected) in [
            ("", base),
            ("https://other.org/z", "https://other.org/z"),
            ("//cdn.example.com/f", "https://cdn.example.com/f"),
            ("/root?a=1&amp;b=2", "https://example.com/root?a=1&b=2"),
            ("?page=2", "https://example.com/a/b?page=2"),
            ("c.pdf", "https://example.com/a/c.pdf"),
        ] {
            let resolved: String = absolute.call((base, href)).unwrap();
            assert_eq!(resolved, expected, "href {href:?}");
        }
    }

    #[test]
    fn read_form_returns_the_absolute_action_and_the_fields_a_browser_would_send() {
        let lua = lua("");

        let (action, user, token, remember, unchecked): (
            String,
            String,
            String,
            String,
            Option<String>,
        ) = eval(
            &lua,
            r##"local page = { url = "https://example.com/login/", body = [[
                 <form id="login" action="check">
                   <input name="user"><input type="hidden" name="token" value="t0k">
                   <input type="checkbox" name="remember" value="on" checked>
                   <input type="checkbox" name="newsletter" value="on">
                 </form>]] }
               local action, fields = fyde.helpers.read_form(page, "#login")
               return action, fields.user, fields.token, fields.remember, fields.newsletter"##,
        );

        assert_eq!(action, "https://example.com/login/check");
        assert_eq!(user, "");
        assert_eq!(token, "t0k");
        assert_eq!(remember, "on");
        assert_eq!(unchecked, None);
    }

    #[test]
    fn submit_form_posts_the_form_with_the_given_values_filled_in() {
        let lua = lua("fyde.http = { post_form = function(url, fields, headers) \
               posted = { url = url, fields = fields, headers = headers } return {} end }");

        let (url, token, user, header, missing_form): (String, String, String, String, bool) = eval(
            &lua,
            r##"local h = fyde.helpers
                   local page = { url = "https://example.com/login", body = [[
                     <form id="f" action="/check">
                       <input type="hidden" name="token" value="t"><input name="user">
                     </form>]] }
                   h.submit_form(page, "#f", { user = "me" }, { ["X-Test"] = "1" })
                   local ok = pcall(h.submit_form, { url = "u", body = "<p></p>" }, "#f", {})
                   return posted.url, posted.fields.token, posted.fields.user,
                     posted.headers["X-Test"], ok"##,
        );

        assert_eq!(url, "https://example.com/check");
        assert_eq!(token, "t");
        assert_eq!(user, "me");
        assert_eq!(header, "1");
        assert!(!missing_form);
    }
}
