use std::sync::Arc;

use mlua::{Lua, Result as LuaResult, Table};
use serde_json::json;
use tracing::Level;

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
    for (name, level) in [
        ("debug", Level::DEBUG),
        ("info", Level::INFO),
        ("warn", Level::WARN),
        ("error", Level::ERROR),
    ] {
        let scraper = scraper_name.to_string();
        let recorder = recorder.clone();
        log.set(
            name,
            lua.create_function(move |_, message: String| {
                // `tracing`'s macros need their level as a constant.
                match level {
                    Level::DEBUG => {
                        tracing::debug!(target: "fyde::scrapers", scraper = %scraper, "{message}")
                    }
                    Level::INFO => {
                        tracing::info!(target: "fyde::scrapers", scraper = %scraper, "{message}")
                    }
                    Level::WARN => {
                        tracing::warn!(target: "fyde::scrapers", scraper = %scraper, "{message}")
                    }
                    _ => tracing::error!(target: "fyde::scrapers", scraper = %scraper, "{message}"),
                }
                recorder.record("log", json!({ "level": name, "message": message }));
                Ok(())
            })?,
        )?;
    }

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

        lua.load(r#"log.debug("a") log.info("b") log.warn("c") log.error("d")"#)
            .exec()
            .unwrap();

        let entries = recorder.entries();
        assert!(entries.iter().all(|entry| entry.event == "log"));
        let values: Vec<_> = entries.into_iter().map(|entry| entry.value).collect();
        assert_eq!(
            values,
            vec![
                serde_json::json!({ "level": "debug", "message": "a" }),
                serde_json::json!({ "level": "info", "message": "b" }),
                serde_json::json!({ "level": "warn", "message": "c" }),
                serde_json::json!({ "level": "error", "message": "d" }),
            ]
        );
    }
}
