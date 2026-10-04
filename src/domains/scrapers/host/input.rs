use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table};
use serde_json::json;

use super::super::reports::Recorder;

/// `fyde.input.ask(question)` blocks until it has an answer to `question` to
/// return, for values that can't be known up front (e.g. an MFA code sent
/// during login). Unlike `demo-rust-fyde`'s `host/input.rs` (which blocks on
/// real stdin), this forwards to `on_question` — see
/// [`crate::ClientConfig::on_scraper_question`] — so the consuming
/// application can ask directly on screen. Fails immediately, rather than
/// blocking forever, if no handler is configured. Writes an `input_ask`
/// entry (question and answer) to `recorder` either way, matching
/// `demo-rust-fyde`'s own `host/input.rs`.
pub(super) fn table(
    lua: &Lua,
    on_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
    recorder: Arc<Recorder>,
) -> LuaResult<Table> {
    let input = lua.create_table()?;

    input.set(
        "ask",
        lua.create_function(move |_, question: String| match &on_question {
            Some(callback) => {
                let response = callback(question.clone());
                recorder.record(
                    "input_ask",
                    json!({ "question": question, "response": response }),
                );
                Ok(response)
            }
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
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set("input", table(&lua, Some(on_question), recorder).unwrap())
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
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set("input", table(&lua, None, recorder).unwrap())
            .unwrap();

        let result: LuaResult<String> = lua.load(r#"return input.ask("anything")"#).eval();

        assert!(result.is_err());
    }

    #[test]
    fn ask_records_the_question_and_answer() {
        let on_question: Arc<dyn Fn(String) -> String + Send + Sync> =
            Arc::new(|_| "42".to_string());

        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set(
                "input",
                table(&lua, Some(on_question), recorder.clone()).unwrap(),
            )
            .unwrap();

        lua.load(r#"input.ask("What is the MFA code?")"#)
            .exec()
            .unwrap();

        let report = recorder.finish();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].event, "input_ask");
        assert_eq!(
            report.entries[0].value,
            serde_json::json!({ "question": "What is the MFA code?", "response": "42" })
        );
    }
}
