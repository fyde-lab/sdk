use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table};
use serde_json::json;
use tokio::runtime::Handle;

use super::super::reports::Recorder;
use super::super::stop::StopSignal;

/// `fyde.input.ask(question)` blocks until it has an answer to `question` to
/// return, for values that can't be known up front (e.g. an MFA code sent
/// during login). Unlike `demo-rust-fyde`'s `host/input.rs` (which blocks on
/// real stdin), this forwards to `on_question` — see
/// [`crate::ClientConfig::on_scraper_question`] — so the consuming
/// application can ask directly on screen. Fails immediately, rather than
/// blocking forever, if no handler is configured. Writes an `input_ask`
/// entry (question and answer) to `recorder` either way, matching
/// `demo-rust-fyde`'s own `host/input.rs`.
///
/// The handler runs on `runtime`'s blocking pool rather than on the run's own
/// thread, so a stopped run (`stop`) stops waiting for the answer at once:
/// the handler itself is left to return in its own time (the caller is
/// expected to dismiss the question once its run ends), and its answer is
/// dropped.
pub(super) fn table(
    lua: &Lua,
    on_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
    recorder: Arc<Recorder>,
    runtime: Handle,
    stop: StopSignal,
) -> LuaResult<Table> {
    let input = lua.create_table()?;

    input.set(
        "ask",
        lua.create_function(move |_, question: String| match &on_question {
            Some(callback) => {
                stop.check()?;
                let callback = callback.clone();
                let asked = question.clone();
                let answer = runtime.spawn_blocking(move || callback(asked));
                let response = stop
                    .block_on(&runtime, answer)?
                    .map_err(mlua::Error::external)?;
                // Only the question: the answer is typically a one-time
                // code, which has no place in a report kept on disk.
                recorder.record("input_ask", json!({ "question": question }));
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
    use std::time::Duration;

    use super::*;
    use crate::ErrorCode;

    fn install(
        lua: &Lua,
        runtime: &tokio::runtime::Runtime,
        on_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
        recorder: Arc<Recorder>,
        stop: StopSignal,
    ) {
        lua.globals()
            .set(
                "input",
                table(lua, on_question, recorder, runtime.handle().clone(), stop).unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn ask_returns_the_callbacks_answer() {
        let on_question: Arc<dyn Fn(String) -> String + Send + Sync> =
            Arc::new(|question| format!("answer to: {question}"));

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        install(
            &lua,
            &runtime,
            Some(on_question),
            recorder,
            StopSignal::new(),
        );

        let result: String = lua
            .load(r#"return input.ask("What is the MFA code?")"#)
            .eval()
            .unwrap();

        assert_eq!(result, "answer to: What is the MFA code?");
    }

    #[test]
    fn ask_fails_immediately_without_a_question_handler_configured() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        install(&lua, &runtime, None, recorder, StopSignal::new());

        let result: LuaResult<String> = lua.load(r#"return input.ask("anything")"#).eval();

        assert!(result.is_err());
    }

    #[test]
    fn ask_records_the_question_but_never_the_answer() {
        let on_question: Arc<dyn Fn(String) -> String + Send + Sync> =
            Arc::new(|_| "42".to_string());

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        install(
            &lua,
            &runtime,
            Some(on_question),
            recorder.clone(),
            StopSignal::new(),
        );

        lua.load(r#"input.ask("What is the MFA code?")"#)
            .exec()
            .unwrap();

        let report = recorder.finish();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].event, "input_ask");
        assert_eq!(
            report.entries[0].value,
            serde_json::json!({ "question": "What is the MFA code?" })
        );
    }

    #[test]
    fn ask_stops_waiting_for_the_answer_once_the_run_is_stopped() {
        // Never answers: only the stop can end the wait.
        let (answer_tx, answer_rx) = std::sync::mpsc::channel::<String>();
        let answer_rx = std::sync::Mutex::new(answer_rx);
        let on_question: Arc<dyn Fn(String) -> String + Send + Sync> =
            Arc::new(move |_| answer_rx.lock().unwrap().recv().unwrap_or_default());

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let lua = Lua::new();
        let stop = StopSignal::new();
        install(
            &lua,
            &runtime,
            Some(on_question),
            Arc::new(Recorder::new("didaxis")),
            stop.clone(),
        );
        runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            stop.stop();
        });

        let err = lua
            .load(r#"return input.ask("What is the MFA code?")"#)
            .eval::<String>()
            .unwrap_err();

        assert_eq!(crate::Error::from(err).code(), ErrorCode::ScraperRunStopped);
        // Lets the abandoned handler return, so the runtime can shut down.
        drop(answer_tx);
    }
}
