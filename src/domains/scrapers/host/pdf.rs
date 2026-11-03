use mlua::{Lua, LuaString, Result as LuaResult, Table};

use crate::domains::documents::transcript::{self, Rect};

/// `fyde.pdf.text(content)` returns the text of every page of the PDF in
/// `content` (e.g. a `fyde.http.download` body) — the same transcript
/// `fyde.save_document` stores with the document. `fyde.pdf.text_in_rect(
/// content, page, { x = ..., y = ..., width = ..., height = ... })` returns
/// only the text of the 1-based `page` that falls, even partly, inside that
/// rectangle: in PDF points (1/72 inch, an A4 page is 595 x 842), from the
/// page's bottom-left corner, `y` growing upwards. Both raise on content
/// that isn't a PDF (and `text_in_rect` on a page it doesn't have).
pub(super) fn table(lua: &Lua) -> LuaResult<Table> {
    let pdf = lua.create_table()?;

    pdf.set(
        "text",
        lua.create_function(|_, content: LuaString| {
            transcript::extract(&content.as_bytes()).map_err(mlua::Error::external)
        })?,
    )?;

    pdf.set(
        "text_in_rect",
        lua.create_function(|_, (content, page, rect): (LuaString, usize, Table)| {
            let page_index = page.checked_sub(1).ok_or_else(|| {
                mlua::Error::external("fyde.pdf.text_in_rect: pages are numbered from 1")
            })?;
            let region = Rect::new(
                rect.get("x")?,
                rect.get("y")?,
                rect.get("width")?,
                rect.get("height")?,
            );
            transcript::extract_in_rect(&content.as_bytes(), page_index, region)
                .map_err(mlua::Error::external)
        })?,
    )?;

    Ok(pdf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::documents::transcript::tests::build_pdf;

    fn lua_with_pdf(text: &str) -> Lua {
        let lua = Lua::new();
        lua.globals().set("pdf", table(&lua).unwrap()).unwrap();
        lua.globals()
            .set("content", lua.create_string(build_pdf(text)).unwrap())
            .unwrap();
        lua
    }

    #[test]
    fn text_returns_the_text_of_the_pdf() {
        let lua = lua_with_pdf("Hello World!");

        let text: String = lua.load("return pdf.text(content)").eval().unwrap();

        assert!(text.contains("Hello World!"));
    }

    #[test]
    fn text_raises_for_content_that_is_not_a_pdf() {
        let lua = lua_with_pdf("Hello World!");

        let result: LuaResult<String> = lua.load(r#"return pdf.text("not a pdf")"#).eval();

        assert!(result.is_err());
    }

    // `build_pdf` writes its text at (100, 600), in 48pt.
    #[test]
    fn text_in_rect_returns_only_the_text_inside_the_rectangle() {
        let lua = lua_with_pdf("Hello World!");

        let (inside, outside): (String, String) = lua
            .load(
                r#"
                return pdf.text_in_rect(content, 1, { x = 90, y = 590, width = 500, height = 70 }),
                  pdf.text_in_rect(content, 1, { x = 0, y = 0, width = 595, height = 100 })
                "#,
            )
            .eval()
            .unwrap();

        assert!(inside.contains("Hello World!"));
        assert!(!outside.contains("Hello"));
    }

    #[test]
    fn text_in_rect_raises_for_page_zero() {
        let lua = lua_with_pdf("Hello World!");

        let result: LuaResult<String> = lua
            .load(
                "return pdf.text_in_rect(content, 0, { x = 0, y = 0, width = 595, height = 842 })",
            )
            .eval();

        assert!(result.is_err());
    }

    #[test]
    fn text_in_rect_raises_for_a_rectangle_missing_a_field() {
        let lua = lua_with_pdf("Hello World!");

        let result: LuaResult<String> = lua
            .load("return pdf.text_in_rect(content, 1, { x = 0, y = 0, width = 595 })")
            .eval();

        assert!(result.is_err());
    }
}
