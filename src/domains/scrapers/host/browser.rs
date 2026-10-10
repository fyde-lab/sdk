use std::sync::Arc;
use std::time::Duration;

use mlua::{Lua, Result as LuaResult, Table};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use super::super::browser::Service as BrowserService;
use super::super::reports::Recorder;

const DEFAULT_WAIT_FOR_TIMEOUT: Duration = Duration::from_secs(10);
// A real file download (even a small PDF) routinely takes longer than a DOM
// change to settle, so `download` gets a longer default than `wait_for`'s.
const DEFAULT_DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// `fyde.browser` — `open`/`wait_for`/`fill`/`click`/`html`/`download`,
/// backed by [`BrowserService`] (a real `wry` webview, see
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
///
/// `allowed_domains` gates `open` the same way it gates every `fyde.http`
/// method (see `host::ensure_domain_allowed`); the other methods act on
/// whatever page is already loaded and never take a URL of their own. Any
/// navigation the page makes on its own afterwards (link clicks, redirects,
/// popups) is gated separately by `browser::driver`'s navigation handlers.
///
/// `debug_http_dump` (forwarded from `Service::run`'s own parameter, the
/// same one that opts `fyde.http` into dumping response bodies — see
/// `host::http::table`'s doc comment) additionally records the page's HTML
/// right after a successful `open`, under the `browser_open` report entry's
/// `html` field — enable it only for a trusted, local debugging run.
pub(super) fn table(
    lua: &Lua,
    browser: Arc<dyn BrowserService>,
    recorder: Arc<Recorder>,
    runtime: Handle,
    debug_http_dump: bool,
    allowed_domains: Arc<Vec<String>>,
) -> LuaResult<Table> {
    let out = lua.create_table()?;

    let open_browser = browser.clone();
    let open_runtime = runtime.clone();
    let open_recorder = recorder.clone();
    out.set(
        "open",
        lua.create_function(move |_, (_self, url): (Table, String)| {
            if let Err(message) = super::ensure_domain_allowed(&url, &allowed_domains) {
                open_recorder.record(
                    "error",
                    json!({ "action": "browser.open", "url": url, "message": message.clone() }),
                );
                return Err(mlua::Error::RuntimeError(message));
            }
            let result = open_runtime.block_on(open_browser.open(&url));
            match &result {
                Ok(()) => {
                    let mut entry = json!({ "url": url });
                    // Mirrors `fyde.http`'s own `debug_http_dump` dump (see
                    // `host::http::table`'s doc comment): opt-in only,
                    // for a trusted local debugging run, since a page's
                    // HTML can carry session-bound content.
                    if debug_http_dump {
                        let html = open_runtime.block_on(open_browser.html());
                        if let Ok(html) = html
                            && let Value::Object(fields) = &mut entry
                        {
                            fields.insert("html".into(), Value::String(html));
                        }
                    }
                    open_recorder.record("browser_open", entry);
                }
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

    let html_browser = browser.clone();
    let html_runtime = runtime.clone();
    let html_recorder = recorder.clone();
    out.set(
        "html",
        lua.create_function(move |_, _self: Table| {
            let result = html_runtime.block_on(html_browser.html());
            match &result {
                Ok(_) => html_recorder.record("browser_html", json!({})),
                Err(err) => html_recorder.record(
                    "error",
                    json!({ "action": "browser.html", "message": err.to_string() }),
                ),
            }
            result.map_err(mlua::Error::external)
        })?,
    )?;

    out.set(
        "download",
        lua.create_function(
            move |lua, (_self, selector, timeout_ms): (Table, String, Option<u64>)| {
                let timeout = timeout_ms
                    .map(Duration::from_millis)
                    .unwrap_or(DEFAULT_DOWNLOAD_TIMEOUT);
                let result = runtime.block_on(browser.download(&selector, timeout));
                match &result {
                    Ok(bytes) => recorder.record(
                        "browser_download",
                        json!({ "selector": selector, "bytes": bytes.len() }),
                    ),
                    Err(err) => recorder.record(
                        "error",
                        json!({
                            "action": "browser.download",
                            "selector": selector,
                            "message": err.to_string(),
                        }),
                    ),
                }
                let bytes = result.map_err(mlua::Error::external)?;
                lua.create_string(&bytes)
            },
        )?,
    )?;

    Ok(out)
}

#[cfg(test)]
mod tests {
    use mockall::predicate::eq;

    use super::*;
    use crate::domains::scrapers::browser::MockService;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    /// Only `open` ever checks `allowed_domains` (see `ensure_domain_allowed`
    /// gating it above) — every other test in this module exercises
    /// `wait_for`/`fill`/`click`/`html`, none of which take a URL, so this
    /// fixed list (matching the one URL `open`'s own tests use) keeps every
    /// other `table(...)` call site here from having to care.
    fn allowed_domains() -> Arc<Vec<String>> {
        Arc::new(vec!["example.com".to_string()])
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
            false,
            allowed_domains(),
        )
        .unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        lua.load(r#"browser:open("https://example.com/login")"#)
            .exec()
            .unwrap();

        let report = recorder.finish();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].event, "browser_open");
        assert!(report.entries[0].value.get("html").is_none());
    }

    #[test]
    fn open_dumps_the_pages_html_when_debug_http_dump_is_set() {
        let mut browser = MockService::new();
        browser
            .expect_open()
            .with(eq("https://example.com/login"))
            .times(1)
            .returning(|_| Ok(()));
        browser
            .expect_html()
            .times(1)
            .returning(|| Ok("<html><body>login page</body></html>".to_string()));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder.clone(),
            runtime.handle().clone(),
            true,
            allowed_domains(),
        )
        .unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        lua.load(r#"browser:open("https://example.com/login")"#)
            .exec()
            .unwrap();

        let report = recorder.finish();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].event, "browser_open");
        assert_eq!(
            report.entries[0].value.get("html").and_then(Value::as_str),
            Some("<html><body>login page</body></html>")
        );
    }

    #[test]
    fn open_blocks_a_url_outside_allowed_domains_without_calling_the_service() {
        let mut browser = MockService::new();
        browser.expect_open().times(0);

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder.clone(),
            runtime.handle().clone(),
            false,
            allowed_domains(),
        )
        .unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        let result: LuaResult<()> = lua.load(r#"browser:open("https://evil.com/login")"#).exec();

        assert!(result.is_err());
        let report = recorder.finish();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].event, "error");
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
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder,
            runtime.handle().clone(),
            false,
            allowed_domains(),
        )
        .unwrap();
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
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder,
            runtime.handle().clone(),
            false,
            allowed_domains(),
        )
        .unwrap();
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
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder,
            runtime.handle().clone(),
            false,
            allowed_domains(),
        )
        .unwrap();
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
            false,
            allowed_domains(),
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
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder,
            runtime.handle().clone(),
            false,
            allowed_domains(),
        )
        .unwrap();
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
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder,
            runtime.handle().clone(),
            false,
            allowed_domains(),
        )
        .unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        let result: LuaResult<()> = lua.load(r##"browser:click("#missing")"##).exec();

        assert!(result.is_err());
    }

    #[test]
    fn html_returns_the_pages_markup_and_records_a_report_entry() {
        let mut browser = MockService::new();
        browser
            .expect_html()
            .returning(|| Ok("<html><body>hi</body></html>".to_string()));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder.clone(),
            runtime.handle().clone(),
            false,
            allowed_domains(),
        )
        .unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        let result: String = lua.load(r##"return browser:html()"##).eval().unwrap();

        assert_eq!(result, "<html><body>hi</body></html>");
        let report = recorder.finish();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].event, "browser_html");
    }

    #[test]
    fn html_propagates_a_service_error() {
        let mut browser = MockService::new();
        browser
            .expect_html()
            .returning(|| Err(crate::Error::Browser("boom".to_string())));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder,
            runtime.handle().clone(),
            false,
            allowed_domains(),
        )
        .unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        let result: LuaResult<String> = lua.load(r##"return browser:html()"##).eval();

        assert!(result.is_err());
    }

    #[test]
    fn download_returns_the_bytes_and_records_their_size_not_their_content() {
        let mut browser = MockService::new();
        browser
            .expect_download()
            .withf(|selector, timeout| {
                selector == "#affichagePdf_123" && *timeout == DEFAULT_DOWNLOAD_TIMEOUT
            })
            .times(1)
            .returning(|_, _| Ok(b"%PDF-1.4 fake bytes".to_vec()));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder.clone(),
            runtime.handle().clone(),
            false,
            allowed_domains(),
        )
        .unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        let result: mlua::LuaString = lua
            .load(r##"return browser:download("#affichagePdf_123")"##)
            .eval()
            .unwrap();

        assert_eq!(result.as_bytes().as_ref(), b"%PDF-1.4 fake bytes");
        let report = recorder.finish();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].event, "browser_download");
        assert_eq!(
            report.entries[0].value.get("bytes").and_then(Value::as_u64),
            Some(19)
        );
        assert!(
            !serde_json::to_string(&report.entries)
                .unwrap()
                .contains("PDF-1.4")
        );
    }

    #[test]
    fn download_passes_a_caller_given_timeout_in_milliseconds() {
        let mut browser = MockService::new();
        browser
            .expect_download()
            .withf(|_, timeout| *timeout == Duration::from_millis(5000))
            .times(1)
            .returning(|_, _| Ok(vec![]));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder,
            runtime.handle().clone(),
            false,
            allowed_domains(),
        )
        .unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        lua.load(r##"browser:download("#affichagePdf_123", 5000)"##)
            .exec()
            .unwrap();
    }

    #[test]
    fn download_propagates_a_service_error() {
        let mut browser = MockService::new();
        browser
            .expect_download()
            .returning(|_, _| Err(crate::Error::Browser("timed out".to_string())));

        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let browser_table = table(
            &lua,
            Arc::new(browser),
            recorder,
            runtime.handle().clone(),
            false,
            allowed_domains(),
        )
        .unwrap();
        lua.globals().set("browser", browser_table).unwrap();

        let result: LuaResult<mlua::LuaString> =
            lua.load(r##"return browser:download("#missing")"##).eval();

        assert!(result.is_err());
    }
}
