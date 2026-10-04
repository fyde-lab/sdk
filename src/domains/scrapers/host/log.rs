use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table};
use serde_json::json;

use super::super::reports::Recorder;

/// `fyde.log.debug/info/warn/error(message)` — forwarded to this crate's
/// `tracing` subscriber (tagged with the scraper's name) rather than printed
/// directly, unlike `demo-rust-fyde`'s `host/log.rs`: it already flows
/// through to [`crate::ClientConfig::on_log`] when set, so a second,
/// scraper-specific callback isn't needed. Each call also writes a `log`
/// entry to `recorder` — see `demo-rust-fyde`'s own `host/logger.rs`, which
/// this is ported from.
pub(super) fn table(lua: &Lua, scraper_name: &str, recorder: Arc<Recorder>) -> LuaResult<Table> {
    let log = lua.create_table()?;

    let scraper = scraper_name.to_string();
    let debug_recorder = recorder.clone();
    log.set(
        "debug",
        lua.create_function(move |_, message: String| {
            tracing::debug!(target: "fyde::scrapers", scraper = %scraper, "{message}");
            debug_recorder.record("log", json!({ "level": "debug", "message": message }));
            Ok(())
        })?,
    )?;

    let scraper = scraper_name.to_string();
    let info_recorder = recorder.clone();
    log.set(
        "info",
        lua.create_function(move |_, message: String| {
            tracing::info!(target: "fyde::scrapers", scraper = %scraper, "{message}");
            info_recorder.record("log", json!({ "level": "info", "message": message }));
            Ok(())
        })?,
    )?;

    let scraper = scraper_name.to_string();
    let warn_recorder = recorder.clone();
    log.set(
        "warn",
        lua.create_function(move |_, message: String| {
            tracing::warn!(target: "fyde::scrapers", scraper = %scraper, "{message}");
            warn_recorder.record("log", json!({ "level": "warn", "message": message }));
            Ok(())
        })?,
    )?;

    let scraper = scraper_name.to_string();
    log.set(
        "error",
        lua.create_function(move |_, message: String| {
            tracing::error!(target: "fyde::scrapers", scraper = %scraper, "{message}");
            recorder.record("log", json!({ "level": "error", "message": message }));
            Ok(())
        })?,
    )?;

    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_level_is_callable_without_a_subscriber_installed() {
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set("log", table(&lua, "didaxis", recorder).unwrap())
            .unwrap();

        lua.load(
            r#"
            log.debug("a debug message")
            log.info("an info message")
            log.warn("a warn message")
            log.error("an error message")
            "#,
        )
        .exec()
        .unwrap();
    }

    #[test]
    fn every_level_records_a_log_entry() {
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        lua.globals()
            .set("log", table(&lua, "didaxis", recorder.clone()).unwrap())
            .unwrap();

        lua.load(r#"log.info("hello")"#).exec().unwrap();

        let report = recorder.finish();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].event, "log");
        assert_eq!(
            report.entries[0].value,
            serde_json::json!({ "level": "info", "message": "hello" })
        );
    }
}
