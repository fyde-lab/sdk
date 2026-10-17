use std::str::FromStr;
use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table};
use serde_json::json;

use super::super::reports::Recorder;
use super::super::{ProgressEvent, ProgressStep};

/// `fyde.progress.step(step)` announces a new high-level stage of the scrape,
/// named by one of [`ProgressStep`]'s [`ProgressStep::as_str`] names — any
/// other value raises a Lua error, since the application only has wording
/// for those. Each name is also exposed as an upper-cased constant
/// (`fyde.progress.LOGGING_IN == "logging_in"`, ...), which scripts use
/// instead of the literal string.
/// `fyde.progress.update(current, total)` reports fine-grained progress
/// within the current step (e.g. 3 of 12 documents downloaded). Neither
/// takes free text: it couldn't be shown in the user's language. Unlike
/// `demo-rust-fyde`'s `host/progress.rs` (which just prints to stdout), both
/// forward to `on_progress` — see
/// [`crate::ClientConfig::on_scraper_progress`] — so the consuming
/// application can render its own progress bar. Both also write a
/// `progress` entry to `recorder`.
pub(super) fn table(
    lua: &Lua,
    on_progress: Option<Arc<dyn Fn(ProgressEvent) + Send + Sync>>,
    recorder: Arc<Recorder>,
) -> LuaResult<Table> {
    let progress = lua.create_table()?;
    for step in ProgressStep::ALL {
        progress.set(step.as_str().to_ascii_uppercase(), step.as_str())?;
    }

    let step_callback = on_progress.clone();
    let step_recorder = recorder.clone();
    progress.set(
        "step",
        lua.create_function(move |_, name: String| {
            let step = ProgressStep::from_str(&name).map_err(mlua::Error::external)?;
            if let Some(callback) = &step_callback {
                callback(ProgressEvent::Step { step });
            }
            step_recorder.record("progress", json!({ "kind": "step", "step": step.as_str() }));
            Ok(())
        })?,
    )?;

    let update_callback = on_progress;
    progress.set(
        "update",
        lua.create_function(move |_, (current, total): (i64, i64)| {
            if let Some(callback) = &update_callback {
                callback(ProgressEvent::Update { current, total });
            }
            recorder.record(
                "progress",
                json!({ "kind": "update", "current": current, "total": total }),
            );
            Ok(())
        })?,
    )?;

    Ok(progress)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    type OnProgress = Arc<dyn Fn(ProgressEvent) + Send + Sync>;

    fn recording_callback() -> (Arc<Mutex<Vec<ProgressEvent>>>, OnProgress) {
        let received = Arc::new(Mutex::new(Vec::new()));
        let callback = received.clone();
        (
            received,
            Arc::new(move |event| callback.lock().unwrap().push(event)),
        )
    }

    #[test]
    fn step_invokes_the_callback_with_the_given_step() {
        let (received, on_progress) = recording_callback();

        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set(
                "progress",
                table(&lua, Some(on_progress), recorder).unwrap(),
            )
            .unwrap();
        lua.load(r#"progress.step("logging_in")"#).exec().unwrap();

        assert_eq!(
            *received.lock().unwrap(),
            vec![ProgressEvent::Step {
                step: ProgressStep::LoggingIn
            }]
        );
    }

    #[test]
    fn every_step_is_exposed_as_an_upper_cased_constant() {
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let progress = table(&lua, None, recorder).unwrap();

        for step in ProgressStep::ALL {
            let name = step.as_str().to_ascii_uppercase();
            assert_eq!(progress.get::<String>(name).unwrap(), step.as_str());
        }
    }

    #[test]
    fn step_accepts_a_constant() {
        let (received, on_progress) = recording_callback();

        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set(
                "progress",
                table(&lua, Some(on_progress), recorder).unwrap(),
            )
            .unwrap();
        lua.load(r#"progress.step(progress.DOWNLOADING_DOCUMENTS)"#)
            .exec()
            .unwrap();

        assert_eq!(
            *received.lock().unwrap(),
            vec![ProgressEvent::Step {
                step: ProgressStep::DownloadingDocuments
            }]
        );
    }

    #[test]
    fn step_rejects_an_unknown_step_without_invoking_the_callback() {
        let (received, on_progress) = recording_callback();

        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set(
                "progress",
                table(&lua, Some(on_progress), recorder).unwrap(),
            )
            .unwrap();
        let result = lua.load(r#"progress.step("Logging in")"#).exec();

        assert!(result.is_err());
        assert!(received.lock().unwrap().is_empty());
    }

    #[test]
    fn update_invokes_the_callback_with_current_and_total() {
        let (received, on_progress) = recording_callback();

        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set(
                "progress",
                table(&lua, Some(on_progress), recorder).unwrap(),
            )
            .unwrap();
        lua.load(r#"progress.update(3, 12)"#).exec().unwrap();

        assert_eq!(
            *received.lock().unwrap(),
            vec![ProgressEvent::Update {
                current: 3,
                total: 12
            }]
        );
    }

    #[test]
    fn calls_are_a_no_op_without_a_callback_configured() {
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set("progress", table(&lua, None, recorder).unwrap())
            .unwrap();

        lua.load(r#"progress.step("done"); progress.update(1, 2)"#)
            .exec()
            .unwrap();
    }

    #[test]
    fn step_and_update_each_record_a_progress_entry() {
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set("progress", table(&lua, None, recorder.clone()).unwrap())
            .unwrap();

        lua.load(r#"progress.step("logging_in"); progress.update(1, 2)"#)
            .exec()
            .unwrap();

        let report = recorder.finish();
        assert_eq!(report.entries.len(), 2);
        assert_eq!(report.entries[0].event, "progress");
        assert_eq!(report.entries[1].event, "progress");
    }
}
