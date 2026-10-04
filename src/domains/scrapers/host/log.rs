use mlua::{Lua, Result as LuaResult, Table};

/// `fyde.log.debug/info/warn/error(message)` — forwarded to this crate's
/// `tracing` subscriber (tagged with the scraper's name) rather than printed
/// directly, unlike `demo-rust-fyde`'s `host/log.rs`: it already flows
/// through to [`crate::ClientConfig::on_log`] when set, so a second,
/// scraper-specific callback isn't needed.
pub(super) fn table(lua: &Lua, scraper_name: &str) -> LuaResult<Table> {
    let log = lua.create_table()?;

    let scraper = scraper_name.to_string();
    log.set(
        "debug",
        lua.create_function(move |_, message: String| {
            tracing::debug!(target: "fyde::scrapers", scraper = %scraper, "{message}");
            Ok(())
        })?,
    )?;

    let scraper = scraper_name.to_string();
    log.set(
        "info",
        lua.create_function(move |_, message: String| {
            tracing::info!(target: "fyde::scrapers", scraper = %scraper, "{message}");
            Ok(())
        })?,
    )?;

    let scraper = scraper_name.to_string();
    log.set(
        "warn",
        lua.create_function(move |_, message: String| {
            tracing::warn!(target: "fyde::scrapers", scraper = %scraper, "{message}");
            Ok(())
        })?,
    )?;

    let scraper = scraper_name.to_string();
    log.set(
        "error",
        lua.create_function(move |_, message: String| {
            tracing::error!(target: "fyde::scrapers", scraper = %scraper, "{message}");
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
        lua.globals()
            .set("log", table(&lua, "didaxis").unwrap())
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
}
