use std::sync::Arc;
use std::time::Duration;

use mlua::{Lua, Result as LuaResult, Table};
use serde_json::json;
use tokio::runtime::Handle;

use super::super::browser::{BrowserResponse, Service as BrowserService};
use super::super::reports::Recorder;

const DEFAULT_WAIT_FOR_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_SUBMIT_TIMEOUT: Duration = Duration::from_secs(30);

/// `fyde.browser` — `open`/`wait_for`/`fill`/`click`/`submit`, backed by
/// [`BrowserService`] (a real `wry` webview, see
/// `browser::driver::BrowserDriver`), so a login flow blocked by a
/// client-rendered SPA or a JS-driven WAF challenge can drive a real
/// browser engine instead of `fyde.http`'s plain HTTP client.
///
/// Unlike `fyde.http`/`fyde.html`'s plain function namespaces, a script is
/// meant to call every method here with Lua's colon syntax —
/// `fyde.browser:open(url)`, not `fyde.browser.open(url)` — since `browser`
/// reads as a single stateful object (one running webview per scraper run)
/// rather than a bag of unrelated functions. Colon calls pass the table
/// itself as an implicit first argument, which is why every function below
/// takes an ignored `Table` as its first parameter.
pub(super) fn table(
    lua: &Lua,
    browser: Arc<dyn BrowserService>,
    recorder: Arc<Recorder>,
    runtime: Handle,
) -> LuaResult<Table> {
    let out = lua.create_table()?;

    let open_browser = browser.clone();
    let open_runtime = runtime.clone();
    let open_recorder = recorder.clone();
    out.set(
        "open",
        lua.create_function(move |_, (_self, url): (Table, String)| {
            let result = open_runtime.block_on(open_browser.open(&url));
            match &result {
                Ok(()) => open_recorder.record("browser_open", json!({ "url": url })),
                Err(err) => open_recorder.record(
                    "error",
                    json!({ "action": "browser.open", "url": url, "message": err.to_string() }),
                ),
            }
            result.map_err(mlua::Error::external)
        })?,
    )?;

    let wait_for_browser = browser.clone();
    let wait_for_runtime = runtime.clone();
    let wait_for_recorder = recorder.clone();
    out.set(
        "wait_for",
        lua.create_function(
            move |_, (_self, selector, timeout_ms): (Table, String, Option<u64>)| {
                let timeout = timeout_ms
                    .map(Duration::from_millis)
                    .unwrap_or(DEFAULT_WAIT_FOR_TIMEOUT);
                let result =
                    wait_for_runtime.block_on(wait_for_browser.wait_for(&selector, timeout));
                match &result {
                    Ok(()) => wait_for_recorder
                        .record("browser_wait_for", json!({ "selector": selector })),
                    Err(err) => wait_for_recorder.record(
                        "error",
                        json!({
                            "action": "browser.wait_for",
                            "selector": selector,
                            "message": err.to_string(),
                        }),
                    ),
                }
                result.map_err(mlua::Error::external)
            },
        )?,
    )?;

    let fill_browser = browser.clone();
    let fill_runtime = runtime.clone();
    let fill_recorder = recorder.clone();
    out.set(
        "fill",
        lua.create_function(
            move |_, (_self, selector, value): (Table, String, String)| {
                let result = fill_runtime.block_on(fill_browser.fill(&selector, &value));
                // Never records the filled-in value itself: a script's second
                // `fill` call is routinely a password (see `fyde.http`'s own
                // `host::http::table` doc comment for the same rule applied to
                // request bodies/headers).
                match &result {
                    Ok(()) => fill_recorder.record("browser_fill", json!({ "selector": selector })),
                    Err(err) => fill_recorder.record(
                        "error",
                        json!({
                            "action": "browser.fill",
                            "selector": selector,
                            "message": err.to_string(),
                        }),
                    ),
                }
                result.map_err(mlua::Error::external)
            },
        )?,
    )?;

    let click_browser = browser.clone();
    let click_runtime = runtime.clone();
    let click_recorder = recorder.clone();
    out.set(
        "click",
        lua.create_function(move |_, (_self, selector): (Table, String)| {
            let result = click_runtime.block_on(click_browser.click(&selector));
            match &result {
                Ok(()) => click_recorder.record("browser_click", json!({ "selector": selector })),
                Err(err) => click_recorder.record(
                    "error",
                    json!({
                        "action": "browser.click",
                        "selector": selector,
                        "message": err.to_string(),
                    }),
                ),
            }
            result.map_err(mlua::Error::external)
        })?,
    )?;

    out.set(
        "submit",
        lua.create_function(
            move |lua, (_self, selector, timeout_ms): (Table, String, Option<u64>)| {
                let timeout = timeout_ms
                    .map(Duration::from_millis)
                    .unwrap_or(DEFAULT_SUBMIT_TIMEOUT);
                let result = runtime.block_on(browser.submit(&selector, timeout));
                match &result {
                    Ok(response) => recorder.record(
                        "browser_submit",
                        json!({ "selector": selector, "status": response.status }),
                    ),
                    Err(err) => recorder.record(
                        "error",
                        json!({
                            "action": "browser.submit",
                            "selector": selector,
                            "message": err.to_string(),
                        }),
                    ),
                }
                match result {
                    Ok(response) => response_to_table(lua, &response),
                    Err(err) => Err(mlua::Error::external(err)),
                }
            },
        )?,
    )?;

    Ok(out)
}

fn response_to_table(lua: &Lua, response: &BrowserResponse) -> LuaResult<Table> {
    let out = lua.create_table()?;
    out.set("status", response.status)?;
    out.set("url", response.url.as_str())?;
    let headers = lua.create_table()?;
    for (name, value) in &response.headers {
        headers.set(name.as_str(), value.as_str())?;
    }
    out.set("headers", headers)?;
    out.set("body", response.body.as_str())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use mockall::predicate::eq;

    use super::*;
    use crate::domains::scrapers::browser::{FakeBrowserResponse, MockService};

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    #[test]
    fn open_calls_the_service_and_records_a_report_entry() {
        let mut browser = MockService::new();
        browser
            .expect_open()
            .with(eq("https://example.com/login"))
            .times(1)
            .returning(|_| Ok(()));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder.clone(),
            runtime.handle().clone(),
        )
        .unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        lua.load(r#"browser:open("https://example.com/login")"#)
            .exec()
            .unwrap();

        let report = recorder.finish();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].event, "browser_open");
    }

    #[test]
    fn wait_for_passes_the_default_timeout_when_none_is_given() {
        let mut browser = MockService::new();
        browser
            .expect_wait_for()
            .withf(|selector, timeout| {
                selector == "#username" && *timeout == DEFAULT_WAIT_FOR_TIMEOUT
            })
            .times(1)
            .returning(|_, _| Ok(()));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table =
            table(&lua, Arc::new(browser), recorder, runtime.handle().clone()).unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        lua.load(r##"browser:wait_for("#username")"##)
            .exec()
            .unwrap();
    }

    #[test]
    fn wait_for_passes_a_caller_given_timeout_in_milliseconds() {
        let mut browser = MockService::new();
        browser
            .expect_wait_for()
            .withf(|selector, timeout| {
                selector == "#username" && *timeout == Duration::from_millis(500)
            })
            .times(1)
            .returning(|_, _| Ok(()));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table =
            table(&lua, Arc::new(browser), recorder, runtime.handle().clone()).unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        lua.load(r##"browser:wait_for("#username", 500)"##)
            .exec()
            .unwrap();
    }

    #[test]
    fn fill_forwards_the_selector_and_value() {
        let mut browser = MockService::new();
        browser
            .expect_fill()
            .withf(|selector, value| selector == "#password" && value == "s3cret")
            .times(1)
            .returning(|_, _| Ok(()));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table =
            table(&lua, Arc::new(browser), recorder, runtime.handle().clone()).unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        lua.load(r##"browser:fill("#password", "s3cret")"##)
            .exec()
            .unwrap();
    }

    #[test]
    fn fill_never_records_the_value_in_the_report() {
        let mut browser = MockService::new();
        browser.expect_fill().returning(|_, _| Ok(()));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder.clone(),
            runtime.handle().clone(),
        )
        .unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        lua.load(r##"browser:fill("#password", "s3cret")"##)
            .exec()
            .unwrap();

        let report = recorder.finish();
        let serialized = serde_json::to_string(&report.entries).unwrap();
        assert!(!serialized.contains("s3cret"));
    }

    #[test]
    fn click_forwards_the_selector() {
        let mut browser = MockService::new();
        browser
            .expect_click()
            .with(eq("#submit-button"))
            .times(1)
            .returning(|_| Ok(()));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table =
            table(&lua, Arc::new(browser), recorder, runtime.handle().clone()).unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        lua.load(r##"browser:click("#submit-button")"##)
            .exec()
            .unwrap();
    }

    #[test]
    fn click_propagates_a_service_error() {
        let mut browser = MockService::new();
        browser
            .expect_click()
            .returning(|_| Err(crate::Error::Browser("element not found".to_string())));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table =
            table(&lua, Arc::new(browser), recorder, runtime.handle().clone()).unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        let result: LuaResult<()> = lua.load(r##"browser:click("#missing")"##).exec();

        assert!(result.is_err());
    }

    #[test]
    fn submit_returns_a_table_with_status_url_headers_and_body() {
        let mut browser = MockService::new();
        browser.expect_submit().returning(|_, _| {
            Ok(FakeBrowserResponse::new()
                .with_status(200)
                .with_body("logged in")
                .build())
        });

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table =
            table(&lua, Arc::new(browser), recorder, runtime.handle().clone()).unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        let result: Table = lua
            .load(r##"return browser:submit("#login-form")"##)
            .eval()
            .unwrap();

        assert_eq!(result.get::<u16>("status").unwrap(), 200);
        assert_eq!(result.get::<String>("body").unwrap(), "logged in");
    }

    #[test]
    fn submit_propagates_a_service_error() {
        let mut browser = MockService::new();
        browser
            .expect_submit()
            .returning(|_, _| Err(crate::Error::Browser("boom".to_string())));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table =
            table(&lua, Arc::new(browser), recorder, runtime.handle().clone()).unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        let result: LuaResult<Table> = lua.load(r##"return browser:submit("#login-form")"##).eval();

        assert!(result.is_err());
    }
}
