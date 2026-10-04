use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table};

use super::super::ProgressEvent;

/// `fyde.progress.step(name)` announces a new high-level stage of the scrape
/// (login, a given document section, ...). `fyde.progress.update(current,
/// total, message)` reports fine-grained progress within the current step
/// (e.g. "3/12 documents downloaded"). Unlike `demo-rust-fyde`'s
/// `host/progress.rs` (which just prints to stdout), both forward to
/// `on_progress` — see [`crate::ClientConfig::on_scraper_progress`] — so the
/// consuming application can render its own progress bar.
pub(super) fn table(
    lua: &Lua,
    on_progress: Option<Arc<dyn Fn(ProgressEvent) + Send + Sync>>,
) -> LuaResult<Table> {
    let progress = lua.create_table()?;

    let step_callback = on_progress.clone();
    progress.set(
        "step",
        lua.create_function(move |_, name: String| {
            if let Some(callback) = &step_callback {
                callback(ProgressEvent::Step { name });
            }
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
                    message,
                });
            }
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
        lua.globals()
            .set("progress", table(&lua, Some(on_progress)).unwrap())
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
        lua.globals()
            .set("progress", table(&lua, Some(on_progress)).unwrap())
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
        lua.globals()
            .set("progress", table(&lua, None).unwrap())
            .unwrap();

        lua.load(r#"progress.step("x"); progress.update(1, 2, "y")"#)
            .exec()
            .unwrap();
    }
}
