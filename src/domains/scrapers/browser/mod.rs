mod driver;
mod models;

pub(super) use models::BrowserResponse;
#[cfg(test)]
pub(super) use models::FakeBrowserResponse;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use crate::Result;

/// Drives a real, embedded webview (`wry`, windowed via `tao`) so a scraper
/// script can get past a site a plain `fyde.http` request can't: a
/// client-rendered SPA with no plain-HTML fallback (e.g. `scaleway`'s
/// invoices table — see `../../CLAUDE.md`), or a WAF/bot-detection
/// challenge (Datadome, etc. — see `boursorama`/`edf`) that only passes
/// when real JS actually runs. Exposed to Lua as `fyde.browser` (see
/// `host::browser`), with the operations a login flow needs:
/// `open`/`wait_for`/`fill`/`click`/`submit`.
///
/// Trait methods take `&self` (not `&mut self`) so implementations can be
/// shared behind `Arc<dyn Service>`, matching every other domain here —
/// even though, unlike those, there's only ever one real implementation
/// ([`driver::BrowserDriver`]): the trait exists so `host::browser`'s own
/// tests can run against a [`MockService`] instead of a real window, the
/// same way `host::documents`'s tests mock `documents::Service` (see the
/// sdk `CLAUDE.md`'s mockall testing convention).
///
/// Unlike `session`/`cookies`, there's no persisted state here and no
/// swappable storage backend — a fresh [`driver::BrowserDriver`] is built
/// per scraper run (see `service.rs::run_script`), lives only for that
/// run, and is torn down (its window closed, its dedicated OS thread
/// joined) once the run's `Arc<dyn Service>` is dropped — see
/// `driver::BrowserDriver`'s `Drop` impl.
///
/// **Caveat verified only on Linux/WebKitGTK**: a real webview needs a live
/// window-system connection (X11/Wayland on Linux; the OS-native webview
/// framework elsewhere) that a headless server or CI runner may simply not
/// have — see [`init`]/`driver::BrowserDriver::ensure_started` for why
/// that's deferred to first use rather than failing every scraper run that
/// never touches `fyde.browser`.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait Service: Send + Sync {
    /// Starts loading `url` in the embedded webview. Doesn't wait for the
    /// page to finish loading — a script is expected to follow up with
    /// [`Service::wait_for`] for whatever element it actually needs, the
    /// same way a real browser's navigation and rendering race a script's
    /// own next step anyway.
    async fn open(&self, url: &str) -> Result<()>;

    /// Blocks until `selector` matches an element in the current page via
    /// `document.querySelector` (checked by polling inside the webview's
    /// own JS engine, not by polling from Rust — see
    /// `driver::wait_for_script`), or until `timeout` elapses.
    async fn wait_for(&self, selector: &str, timeout: Duration) -> Result<()>;

    /// Sets the value of the first element matched by `selector` and
    /// dispatches `input`/`change` events on it, the way a real keystroke
    /// would — so a framework that only reacts to those events, rather
    /// than reading `.value` directly on submit, still sees the change.
    async fn fill(&self, selector: &str, value: &str) -> Result<()>;

    /// Clicks the first element matched by `selector` via its DOM `.click()`
    /// method — the standard way to simulate a click on a button, link, or
    /// checkbox without a real pointer/window-system event, and (unlike
    /// [`Service::submit`]) doesn't assume the target is a `<form>` or wait
    /// for any resulting navigation/fetch to finish.
    async fn click(&self, selector: &str) -> Result<()>;

    /// Submits the `<form>` matched by `selector` via an in-page `fetch()`
    /// (not a real navigation), so the script gets the response back as
    /// data instead of losing it to a page load. Cookies set along the way
    /// are still picked up by the webview's own cookie store
    /// (`credentials: 'include'`) — but that store is entirely separate
    /// from `fyde.http`'s own `wreq` cookie jar; nothing here copies
    /// cookies between the two, so a script that logs in via `fyde.browser`
    /// and then wants `fyde.http` to reuse that session has nothing built
    /// in to do so yet.
    async fn submit(&self, selector: &str, timeout: Duration) -> Result<BrowserResponse>;

    /// Returns the current page's full `document.documentElement.outerHTML`
    /// — the one way a script (or a developer debugging one) can see what
    /// the webview actually rendered, since every other method here only
    /// reports a selector match/mismatch, never the markup itself.
    async fn html(&self) -> Result<String>;
}

/// Builds the real, `wry`-backed browser service. Never fails on its own —
/// creating the actual window/webview is deferred to first use (see
/// `driver::BrowserDriver::ensure_started`), so a scraper run that never
/// calls into `fyde.browser` never pays the cost, or the risk, of touching
/// a display at all. `allowed_domains` is the same list `host::install`
/// enforces on `fyde.browser:open`'s own URL — here it also bounds every
/// navigation the webview makes on its own afterwards (a link click, a form
/// submit that does a real navigation, a JS `location` change, a `target`
/// attribute or `window.open` popup), via `driver::BrowserDriver`'s
/// `with_navigation_handler`/`with_new_window_req_handler`.
pub(super) fn init(allowed_domains: Arc<Vec<String>>) -> Arc<dyn Service> {
    Arc::new(driver::BrowserDriver::new(allowed_domains))
}
