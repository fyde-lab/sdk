mod driver;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

use super::cookies::Cookie;
use crate::Result;

/// Drives a real, embedded webview (`wry`, windowed via `tao`) so a scraper
/// script can get past a site a plain `fyde.http` request can't: a
/// client-rendered SPA with no plain-HTML fallback (e.g. `scaleway`'s
/// invoices table — see the `scripts` repo's `CLAUDE.md`), or a
/// WAF/bot-detection challenge (Datadome, etc. — see `boursorama`/`edf`)
/// that only passes when real JS actually runs. Exposed to Lua as
/// `fyde.browser` (see `host::browser`), with the operations a login flow
/// needs: `open`/`wait_for`/`fill`/`click`/`html`/`download`.
///
/// Trait methods take `&self` (not `&mut self`) so implementations can be
/// shared behind `Arc<dyn Service>`, matching every other domain here —
/// even though, unlike those, there's only ever one real implementation
/// ([`driver::BrowserDriver`]): the trait exists so `host::browser`'s own
/// tests can run against a [`MockService`] instead of a real window, the
/// same way `host::documents`'s tests mock `documents::Service` (see the
/// sdk `CLAUDE.md`'s mockall testing convention).
///
/// Unlike `session`/`cookies`, there's no swappable storage backend, and the
/// only persisted state is cookies, which go through the `cookies`
/// sub-domain (see [`init`]/[`Service::cookies`]) rather than any storage of
/// this module's own — a fresh [`driver::BrowserDriver`] is built
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
    /// than reading `.value` directly, still sees the change.
    async fn fill(&self, selector: &str, value: &str) -> Result<()>;

    /// Clicks the first element matched by `selector` via its DOM `.click()`
    /// method — the standard way to simulate a click on a button, link, or
    /// checkbox without a real pointer/window-system event, and doesn't
    /// assume the target is a `<form>` or wait for any resulting
    /// navigation/fetch to finish. The only way a script submits a form
    /// here — there's deliberately no in-page-`fetch()` submit path, since
    /// that would bypass the page's own submit handling and break any
    /// bot-mitigation/fingerprinting script hooked onto a real submit event.
    async fn click(&self, selector: &str) -> Result<()>;

    /// Returns the current page's full `document.documentElement.outerHTML`
    /// — the one way a script (or a developer debugging one) can see what
    /// the webview actually rendered, since every other method here only
    /// reports a selector match/mismatch, never the markup itself.
    async fn html(&self) -> Result<String>;

    /// Clicks the first element matched by `selector`, the same way
    /// [`Service::click`] does, but for a button/link whose handler doesn't
    /// change the page at all — instead it triggers the webview engine's own
    /// native file-download machinery (a `window.open()`/popup whose
    /// response carries `Content-Disposition: attachment`, or an anchor with
    /// a `download` attribute), which neither [`Service::wait_for`] nor
    /// [`Service::html`] can ever observe since nothing in the DOM changes
    /// (confirmed live against `cesu.urssaf.fr`'s "Bulletin de salaire"
    /// buttons — see the `scripts` repo's `scrapers/cesu/script.lua` header
    /// comment). Blocks until the download finishes (or `timeout` elapses)
    /// and returns the downloaded file's raw bytes, read back from wherever
    /// the webview engine saved it — see `driver::BrowserDriver`'s
    /// `download_started_handler`/`download_completed_handler` for how that
    /// destination is chosen and captured. Only one `download` call may be
    /// in flight at a time per driver (matching every scraper script's own
    /// single-threaded, one-step-at-a-time use of `fyde.browser`).
    async fn download(&self, selector: &str, timeout: Duration) -> Result<Vec<u8>>;

    /// Returns every cookie the webview currently holds for a host in
    /// `allowed_domains`, as [`Cookie`]s the `cookies` sub-domain can persist
    /// — the counterpart of the saved cookies [`init`] seeds it with, so a
    /// browser-driven login survives across runs the same way a `fyde.http`
    /// one does. Every such cookie is host-only (see
    /// `driver::to_webview_cookie` for why). Empty, without ever starting
    /// the webview, if this run never used it.
    async fn cookies(&self) -> Result<Vec<Cookie>>;
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
///
/// `visible` toggles whether the window this run's webview is embedded in
/// is actually shown, so a scraper run only pops up a real window when a
/// caller explicitly asks to watch (or manually intervene in) its
/// browser-driven flow.
///
/// `cookies` (this scraper's saved cookie jar, see the `cookies`
/// sub-domain) is loaded into the webview as soon as it starts, before
/// anything navigates — see [`Service::cookies`] for the way back out.
pub(super) fn init(
    allowed_domains: Arc<Vec<String>>,
    visible: bool,
    cookies: Vec<Cookie>,
) -> Arc<dyn Service> {
    Arc::new(driver::BrowserDriver::new(
        allowed_domains,
        visible,
        cookies,
    ))
}
