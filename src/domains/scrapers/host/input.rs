use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table};

/// `fyde.input.ask(question)` blocks until it has an answer to `question` to
/// return, for values that can't be known up front (e.g. an MFA code sent
/// during login). Unlike `demo-rust-fyde`'s `host/input.rs` (which blocks on
/// real stdin), this forwards to `on_question` — see
/// [`crate::ClientConfig::on_scraper_question`] — so the consuming
/// application can ask directly on screen. Fails immediately, rather than
/// blocking forever, if no handler is configured.
pub(super) fn table(
    lua: &Lua,
    on_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
) -> LuaResult<Table> {
    let input = lua.create_table()?;

    input.set(
        "ask",
        lua.create_function(move |_, question: String| match &on_question {
            Some(callback) => Ok(callback(question)),
            None => Err(mlua::Error::RuntimeError(
                "fyde.input.ask was called but no question handler is configured for this scraper run (see ClientConfig::on_scraper_question)".to_string(),
            )),
        })?,
    )?;

    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_returns_the_callbacks_answer() {
        let on_question: Arc<dyn Fn(String) -> String + Send + Sync> =
            Arc::new(|question| format!("answer to: {question}"));

        let lua = Lua::new();
        lua.globals()
            .set("input", table(&lua, Some(on_question)).unwrap())
            .unwrap();

        let result: String = lua
            .load(r#"return input.ask("What is the MFA code?")"#)
            .eval()
            .unwrap();

        assert_eq!(result, "answer to: What is the MFA code?");
    }

    #[test]
    fn ask_fails_immediately_without_a_question_handler_configured() {
        let lua = Lua::new();
        lua.globals()
            .set("input", table(&lua, None).unwrap())
            .unwrap();

        let result: LuaResult<String> = lua.load(r#"return input.ask("anything")"#).eval();

        assert!(result.is_err());
    }
}
