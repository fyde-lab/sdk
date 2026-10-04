use mlua::{Lua, Result as LuaResult, Table};
use scraper::{Html, Selector};

/// `fyde.html.select(html, css_selector)` parses `html` and returns an array
/// of tables, one per matching element: `{ text = "...", html = "<outer
/// html>", attrs = { href = "...", ... } }`. Documents are re-parsed on every
/// call rather than kept as long-lived handles — pages here are small, and it
/// sidesteps `scraper`'s self-referential `ElementRef` lifetimes entirely.
/// Ported from `demo-rust-fyde`'s `host/html.rs` (minus its debug report).
pub(super) fn table(lua: &Lua) -> LuaResult<Table> {
    let html = lua.create_table()?;

    html.set(
        "select",
        lua.create_function(move |lua, (document, selector): (String, String)| {
            let parsed = Html::parse_document(&document);
            let parsed_selector = Selector::parse(&selector)
                .map_err(|err| mlua::Error::external(format!("invalid CSS selector: {err:?}")))?;

            let results = lua.create_table()?;
            for (i, element) in parsed.select(&parsed_selector).enumerate() {
                let row = lua.create_table()?;

                let text: String = element
                    .text()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                row.set("text", text)?;
                row.set("html", element.html())?;

                let attrs = lua.create_table()?;
                for (name, value) in element.value().attrs() {
                    attrs.set(name, value)?;
                }
                row.set("attrs", attrs)?;

                results.set(i + 1, row)?;
            }

            Ok(results)
        })?,
    )?;

    Ok(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_returns_text_html_and_attrs_for_each_match() {
        let lua = Lua::new();
        lua.globals().set("html", table(&lua).unwrap()).unwrap();

        let result: Table = lua
            .load(r#"return html.select('<a href="/a">One</a><a href="/b">Two</a>', "a[href]")"#)
            .eval()
            .unwrap();

        assert_eq!(result.raw_len(), 2);
        let first: Table = result.get(1).unwrap();
        assert_eq!(first.get::<String>("text").unwrap(), "One");
        let attrs: Table = first.get("attrs").unwrap();
        assert_eq!(attrs.get::<String>("href").unwrap(), "/a");
    }

    #[test]
    fn select_returns_an_empty_table_when_nothing_matches() {
        let lua = Lua::new();
        lua.globals().set("html", table(&lua).unwrap()).unwrap();

        let result: Table = lua
            .load(r#"return html.select("<p>no links here</p>", "a[href]")"#)
            .eval()
            .unwrap();

        assert_eq!(result.raw_len(), 0);
    }

    #[test]
    fn select_rejects_an_invalid_css_selector() {
        let lua = Lua::new();
        lua.globals().set("html", table(&lua).unwrap()).unwrap();

        let result: LuaResult<Table> = lua.load(r#"return html.select("<p></p>", "[[[")"#).eval();

        assert!(result.is_err());
    }
}
