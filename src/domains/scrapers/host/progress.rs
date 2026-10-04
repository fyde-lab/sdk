use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table};
use serde_json::json;

use super::super::ProgressEvent;
use super::super::reports::Recorder;

/// `fyde.progress.step(name)` announces a new high-level stage of the scrape
/// (login, a given document section, ...). `fyde.progress.update(current,
/// total, message)` reports fine-grained progress within the current step
/// (e.g. "3/12 documents downloaded"). Unlike `demo-rust-fyde`'s
/// `host/progress.rs` (which just prints to stdout), both forward to
/// `on_progress` — see [`crate::ClientConfig::on_scraper_progress`] — so the
/// consuming application can render its own progress bar. Both also write a
/// `progress` entry to `recorder`.
pub(super) fn table(
    lua: &Lua,
    on_progress: Option<Arc<dyn Fn(ProgressEvent) + Send + Sync>>,
    recorder: Arc<Recorder>,
) -> LuaResult<Table> {
    let progress = lua.create_table()?;

    let step_callback = on_progress.clone();
    let step_recorder = recorder.clone();
    progress.set(
        "step",
        lua.create_function(move |_, name: String| {
            if let Some(callback) = &step_callback {
                callback(ProgressEvent::Step { name: name.clone() });
            }
            step_recorder.record("progress", json!({ "kind": "step", "name": name }));
            Ok(())
        })?,
    )?;

    let update_callback = on_progress;
    progress.set(
        "update",
        lua.create_function(move |_, (current, total, message): (i64, i64, String)| {
            if let Some(callback) = &update_callback {
                callback(ProgressEvent::Update {
                    current,
                    total,
                    message: message.clone(),
                });
            }
            recorder.record(
                "progress",
                json!({ "kind": "update", "current": current, "total": total, "message": message }),
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

    #[test]
    fn step_invokes_the_callback_with_the_given_name() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let callback = received.clone();
        let on_progress: Arc<dyn Fn(ProgressEvent) + Send + Sync> =
            Arc::new(move |event| callback.lock().unwrap().push(event));

        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set(
                "progress",
                table(&lua, Some(on_progress), recorder).unwrap(),
            )
            .unwrap();
        lua.load(r#"progress.step("Logging in")"#).exec().unwrap();

        assert_eq!(
            *received.lock().unwrap(),
            vec![ProgressEvent::Step {
                name: "Logging in".to_string()
            }]
        );
    }

    #[test]
    fn update_invokes_the_callback_with_current_total_and_message() {
        let received = Arc::new(Mutex::new(Vec::new()));
        let callback = received.clone();
        let on_progress: Arc<dyn Fn(ProgressEvent) + Send + Sync> =
            Arc::new(move |event| callback.lock().unwrap().push(event));

        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set(
                "progress",
                table(&lua, Some(on_progress), recorder).unwrap(),
            )
            .unwrap();
        lua.load(r#"progress.update(3, 12, "payslip.pdf")"#)
            .exec()
            .unwrap();

        assert_eq!(
            *received.lock().unwrap(),
            vec![ProgressEvent::Update {
                current: 3,
                total: 12,
                message: "payslip.pdf".to_string()
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

        lua.load(r#"progress.step("x"); progress.update(1, 2, "y")"#)
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

        lua.load(r#"progress.step("Logging in"); progress.update(1, 2, "halfway")"#)
            .exec()
            .unwrap();

        let report = recorder.finish();
        assert_eq!(report.entries.len(), 2);
        assert_eq!(report.entries[0].event, "progress");
        assert_eq!(report.entries[1].event, "progress");
    }
}
