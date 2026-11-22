use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine as _;
use rand::RngExt as _;
use serde_json::Value;
use tokio::sync::oneshot;

use crate::{ErrorKind, Result};

use super::super::cookies::Cookie;
use super::super::host::{MAX_BODY_BYTES, is_host_allowed};
use super::super::reports::redact_url;
use super::Service;
use super::engine::{
    WebviewConfig, WebviewCookie, WebviewEngine, WebviewHandler, WebviewSameSite, WebviewSession,
};

/// One pending `call`'s reply slot, keyed by the id embedded in the JS it
/// sent — see [`BrowserDriver::call`]. Shared between the calling task
/// (which inserts, then awaits the receiving half) and
/// [`DriverHandler::on_message`] (which looks the id up and fills it in once
/// the page posts a matching message back).
type PendingReplies = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

/// The single in-flight download's reply slot, if any — set by
/// [`BrowserDriver::download`] right before it triggers the click that
/// starts the download, and filled in with the download's URL by
/// [`DriverHandler::on_download`] (or, for a popup,
/// [`DriverHandler::allow_new_window`]). `None` here means no script-level
/// `download` call is currently waiting, which can happen if a page starts
/// a download on its own outside of `download`'s control — that download is
/// just cancelled. Only one slot, not a map keyed by id like
/// [`PendingReplies`], since nothing here runs more than one download at a
/// time (see [`Service::download`]'s own doc comment).
type PendingDownload = Arc<Mutex<Option<oneshot::Sender<String>>>>;

/// What a run's webview asks its driver, from whatever thread its engine
/// calls from: every rule `fyde.browser` enforces on a page lives here, not
/// in the engine.
struct DriverHandler {
    allowed_domains: Arc<Vec<String>>,
    pending: PendingReplies,
    pending_download: PendingDownload,
    /// Set once the webview went away on its own (its window closed by the
    /// user), so later calls fail at once.
    closed: AtomicBool,
}

impl DriverHandler {
    /// Hands `url` to the `download` call waiting in `pending_download`, if
    /// any, emptying the slot.
    fn deliver_download_url(&self, url: String) {
        let waiting = self
            .pending_download
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        match waiting {
            Some(sender) => {
                let _ = sender.send(url);
            }
            None => tracing::warn!(
                url = %redact_url(&url),
                "fyde.browser: cancelled a download no `download` call was waiting for"
            ),
        }
    }
}

impl WebviewHandler for DriverHandler {
    fn on_message(&self, body: String) {
        let Ok(parsed) = serde_json::from_str::<Value>(&body) else {
            return;
        };
        let Some(id) = parsed.get("id").and_then(Value::as_u64) else {
            return;
        };
        let sender = self
            .pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        if let Some(sender) = sender {
            let _ = sender.send(parsed);
        }
    }

    /// Gates every navigation the webview makes on its own after `open`'s
    /// initial load — a link click, a form submit that does a real
    /// navigation, a JS `location` change, a server redirect — against the
    /// same `allowed_domains` list `fyde.browser:open` itself is checked
    /// against (`host::browser`).
    fn allow_navigation(&self, url: &str) -> bool {
        let allowed = is_navigation_allowed(url, &self.allowed_domains);
        if !allowed {
            tracing::warn!(
                url = %redact_url(url),
                "browser navigation blocked by allowed_domains"
            );
        }
        allowed
    }

    /// Same check for a `window.open(...)`/`target="_blank"` popup.
    ///
    /// While a `Service::download` call is waiting, an allowed popup is
    /// denied and its URL handed to that call instead: a page that opens
    /// its PDF in a popup (cesu's `affichagePdf_<id>` buttons) would
    /// otherwise just display it there (on Linux, in a separate, always
    /// shown window) and `download` would time out.
    fn allow_new_window(&self, url: &str) -> bool {
        if !is_navigation_allowed(url, &self.allowed_domains) {
            tracing::warn!(
                url = %redact_url(url),
                "browser popup blocked by allowed_domains"
            );
            return false;
        }
        let download_waiting = self
            .pending_download
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some();
        if download_waiting {
            self.deliver_download_url(url.to_string());
            return false;
        }
        true
    }

    /// The engine cancelled the native download before writing anything to
    /// disk: the waiting `Service::download` call fetches it from inside
    /// the page instead (see [`fetch_script`]), so the bytes only ever
    /// exist in memory.
    fn on_download(&self, url: &str) {
        self.deliver_download_url(url.to_string());
    }

    /// Drops every reply slot still waiting, so a driver call blocked on
    /// one fails at once instead of running into its timeout.
    fn on_closed(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        self.pending_download
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
    }
}

fn window_closed() -> crate::Error {
    ErrorKind::Browser("the browser window was closed".to_string()).into()
}

/// The real [`Service`] implementation, over whichever [`WebviewEngine`]
/// the platform has: opens its session on first use (see
/// [`ensure_started`](Self::ensure_started)) and closes it when the driver
/// is dropped. `wait_for`/`fill`/`click`/`html` evaluate a JS snippet
/// carrying a unique id that the page's `window.ipc` eventually echoes back
/// — see [`call`](Self::call).
pub(super) struct BrowserDriver {
    engine: Arc<dyn WebviewEngine>,
    session: Mutex<Option<Box<dyn WebviewSession>>>,
    handler: Arc<DriverHandler>,
    visible: bool,
    initial_cookies: Vec<Cookie>,
}

impl BrowserDriver {
    pub(super) fn new(
        engine: Arc<dyn WebviewEngine>,
        allowed_domains: Arc<Vec<String>>,
        visible: bool,
        initial_cookies: Vec<Cookie>,
    ) -> Self {
        Self {
            engine,
            session: Mutex::new(None),
            handler: Arc::new(DriverHandler {
                allowed_domains,
                pending: Arc::new(Mutex::new(HashMap::new())),
                pending_download: Arc::new(Mutex::new(None)),
                closed: AtomicBool::new(false),
            }),
            visible,
            initial_cookies,
        }
    }

    /// A fresh, unguessable id for one `call` round-trip. The handler
    /// accepts a reply from any script running in the page, so a sequential
    /// id would let any of its scripts answer a pending call blind (e.g. a
    /// forged `html` or download). It can't stop page JS that hooks
    /// `window.ipc.postMessage` to rewrite our own replies — the snippets
    /// run in the page's realm. 53 bits, so it survives the trip through a
    /// JS number unchanged.
    fn next_id(&self) -> u64 {
        rand::rng().random::<u64>() >> 11
    }

    /// Opens this run's webview the first time any [`Service`] method is
    /// actually called — never at construction. A scraper script that only
    /// ever calls `fyde.http`/`fyde.html` never pays the cost, or the risk,
    /// of creating a real webview: on desktop it needs a live window-system
    /// connection (X11/Wayland on Linux) that a headless server or CI
    /// runner may simply not have, and this way that's only ever a problem
    /// for a run that actually touches `fyde.browser`. Then runs `action`
    /// on the session, with the lock held, so `Drop` can't close it under
    /// a call.
    fn with_session<T>(&self, action: impl FnOnce(&dyn WebviewSession) -> Result<T>) -> Result<T> {
        if self.handler.closed.load(Ordering::SeqCst) {
            return Err(window_closed());
        }

        let mut guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        if guard.is_none() {
            let config = WebviewConfig {
                visible: self.visible,
                cookies: self
                    .initial_cookies
                    .iter()
                    .filter_map(|saved| {
                        let cookie = to_webview_cookie(saved);
                        if cookie.is_none() {
                            tracing::warn!(
                                origin = %saved.origin,
                                "fyde.browser: skipping an unparseable saved cookie"
                            );
                        }
                        cookie
                    })
                    .collect(),
            };
            *guard = Some(self.engine.open_session(config, self.handler.clone())?);
        }
        let session = guard.as_deref().ok_or_else(window_closed)?;
        action(session)
    }

    /// Opens the session if it isn't yet, without doing anything with it.
    fn ensure_started(&self) -> Result<()> {
        self.with_session(|_| Ok(()))
    }

    /// Runs one round-trip: allocates an id, registers its reply slot in
    /// `PendingReplies` *before* evaluating the script (so the handler can
    /// never receive a reply for an id nothing is waiting on yet), builds
    /// the script via `build_js`, evaluates it, and awaits the reply (or
    /// `timeout`) without blocking the thread, so a stopped run gives up on
    /// it at once. Used by `wait_for`/`fill`/`click`/`html` — `open` has no
    /// reply to wait for, so it bypasses this.
    async fn call(&self, build_js: impl FnOnce(u64) -> String, timeout: Duration) -> Result<Value> {
        self.ensure_started()?;
        let id = self.next_id();
        let pending = &self.handler.pending;

        let (tx, rx) = oneshot::channel();
        pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, tx);
        // Removes the slot however this call ends, its future dropped by a
        // stopped run (see `StopSignal::block_on`) included.
        let _slot = ReplySlot { pending, id };
        // The window may have closed since `ensure_started` checked, clearing
        // `pending` before this slot went in: nothing would ever answer it.
        if self.handler.closed.load(Ordering::SeqCst) {
            return Err(window_closed());
        }

        let script = build_js(id);
        self.with_session(|session| session.evaluate_script(&script))?;

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(window_closed()),
            Err(_) => Err(ErrorKind::Browser(format!(
                "timed out after {timeout:?} waiting for the browser"
            ))
            .into()),
        }
    }

    fn set_pending_download(&self, slot: Option<oneshot::Sender<String>>) {
        *self
            .handler
            .pending_download
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = slot;
    }
}

/// Removes a [`BrowserDriver::call`]'s reply slot from `pending` when
/// dropped.
struct ReplySlot<'a> {
    pending: &'a PendingReplies,
    id: u64,
}

impl Drop for ReplySlot<'_> {
    fn drop(&mut self) {
        self.pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.id);
    }
}

/// Empties a [`BrowserDriver::download`]'s slot in `pending_download` when
/// dropped, so a download given up on (timed out, failed, or its future
/// dropped by a stopped run) never takes a later popup.
struct DownloadSlot<'a>(&'a BrowserDriver);

impl Drop for DownloadSlot<'_> {
    fn drop(&mut self) {
        self.0.set_pending_download(None);
    }
}

impl Drop for BrowserDriver {
    fn drop(&mut self) {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(session) = session {
            session.close();
        }
    }
}

#[async_trait]
impl Service for BrowserDriver {
    async fn open(&self, url: &str) -> Result<()> {
        self.with_session(|session| session.load_url(url))
    }

    async fn wait_for(&self, selector: &str, timeout: Duration) -> Result<()> {
        let timeout_ms = timeout.as_millis() as u64;
        let value = self
            .call(
                |id| wait_for_script(id, selector, timeout_ms),
                timeout + Duration::from_secs(1),
            )
            .await?;
        parse_ack(value)
    }

    async fn fill(&self, selector: &str, value: &str) -> Result<()> {
        let result = self
            .call(
                |id| fill_script(id, selector, value),
                Duration::from_secs(10),
            )
            .await?;
        parse_ack(result)
    }

    async fn click(&self, selector: &str) -> Result<()> {
        let result = self
            .call(|id| click_script(id, selector), Duration::from_secs(10))
            .await?;
        parse_ack(result)
    }

    async fn html(&self) -> Result<String> {
        let value = self.call(html_script, Duration::from_secs(10)).await?;
        parse_html(value)
    }

    async fn cookies(&self) -> Result<Vec<Cookie>> {
        // Deliberately not `with_session`: a run that never touched
        // `fyde.browser` has no cookies here, and mustn't open a webview
        // just to find that out.
        let guard = self.session.lock().unwrap_or_else(|p| p.into_inner());
        let Some(session) = guard.as_deref() else {
            return Ok(Vec::new());
        };
        if self.handler.closed.load(Ordering::SeqCst) {
            return Err(window_closed());
        }
        Ok(session
            .cookies()?
            .iter()
            .filter_map(|cookie| from_webview_cookie(cookie, &self.handler.allowed_domains))
            .collect())
    }

    async fn download(&self, selector: &str, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        self.ensure_started()?;

        let (tx, rx) = oneshot::channel();
        self.set_pending_download(Some(tx));
        let _slot = DownloadSlot(self);

        // Reuses the same click path `Service::click` does — the download
        // is a side effect of this click, not a separate webview command.
        self.click(selector).await?;

        let Ok(Ok(url)) =
            tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), rx).await
        else {
            return Err(ErrorKind::Browser(format!(
                "timed out after {timeout:?} waiting for a download to start"
            ))
            .into());
        };
        if !is_download_allowed(&url, &self.handler.allowed_domains) {
            return Err(ErrorKind::Browser(format!(
                "download url {:?} is not in this scraper's allowed_domains list",
                redact_url(&url)
            ))
            .into());
        }

        let value = self
            .call(
                |id| fetch_script(id, &url),
                deadline.saturating_duration_since(Instant::now()),
            )
            .await?;
        parse_fetched(value)
    }
}

/// JS that polls `document.querySelector(selector)` from inside the
/// webview's own engine (rather than Rust re-evaluating a check script on a
/// timer) until it matches or `timeout_ms` elapses, then reports success or
/// failure back over `window.ipc.postMessage`. Both `selector` and every
/// other value interpolated into this and the other `*_script` functions
/// below goes through `serde_json::to_string` to produce a safely-escaped
/// JS string/number literal — never raw string interpolation of
/// script-controlled content into the JS source.
fn wait_for_script(id: u64, selector: &str, timeout_ms: u64) -> String {
    let selector_json = json_literal(selector);
    format!(
        r#"(function(){{
            var __id = {id};
            var __deadline = Date.now() + {timeout_ms};
            function __check(){{
                try {{
                    if (document.querySelector({selector_json})) {{
                        window.ipc.postMessage(JSON.stringify({{id: __id, ok: true}}));
                        return;
                    }}
                }} catch (e) {{
                    window.ipc.postMessage(JSON.stringify({{id: __id, ok: false, error: String(e)}}));
                    return;
                }}
                if (Date.now() > __deadline) {{
                    window.ipc.postMessage(JSON.stringify({{id: __id, ok: false, error: "timed out waiting for selector"}}));
                }} else {{
                    setTimeout(__check, 100);
                }}
            }}
            __check();
        }})();"#
    )
}

/// JS that sets the matched element's `.value` and dispatches `input`/
/// `change` events on it (see [`Service::fill`]'s doc comment for why),
/// then reports success or failure back over IPC.
fn fill_script(id: u64, selector: &str, value: &str) -> String {
    let selector_json = json_literal(selector);
    let value_json = json_literal(value);
    format!(
        r#"(function(){{
            var __id = {id};
            try {{
                var el = document.querySelector({selector_json});
                if (!el) {{
                    window.ipc.postMessage(JSON.stringify({{id: __id, ok: false, error: "element not found"}}));
                    return;
                }}
                // Go through the prototype's native setter rather than
                // `el.value = ...`: React (and libraries built on it, like
                // react-hook-form) tracks an input's last-known value via an
                // instance-level setter, so a plain assignment updates that
                // tracker too and React then ignores the `input` event below
                // as a no-op, leaving its own state empty.
                var proto = el instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype
                    : el instanceof HTMLSelectElement ? HTMLSelectElement.prototype
                    : HTMLInputElement.prototype;
                Object.getOwnPropertyDescriptor(proto, 'value').set.call(el, {value_json});
                el.dispatchEvent(new Event('input', {{bubbles: true}}));
                el.dispatchEvent(new Event('change', {{bubbles: true}}));
                window.ipc.postMessage(JSON.stringify({{id: __id, ok: true}}));
            }} catch (e) {{
                window.ipc.postMessage(JSON.stringify({{id: __id, ok: false, error: String(e)}}));
            }}
        }})();"#
    )
}

/// JS that calls `.click()` on the matched element (see [`Service::click`]'s
/// doc comment for why that, rather than synthesizing pointer events), then
/// reports success or failure back over IPC.
fn click_script(id: u64, selector: &str) -> String {
    let selector_json = json_literal(selector);
    format!(
        r#"(function(){{
            var __id = {id};
            try {{
                var el = document.querySelector({selector_json});
                if (!el) {{
                    window.ipc.postMessage(JSON.stringify({{id: __id, ok: false, error: "element not found"}}));
                    return;
                }}
                el.click();
                window.ipc.postMessage(JSON.stringify({{id: __id, ok: true}}));
            }} catch (e) {{
                window.ipc.postMessage(JSON.stringify({{id: __id, ok: false, error: String(e)}}));
            }}
        }})();"#
    )
}

/// JS that reports the current page's full `document.documentElement.outerHTML`
/// back over IPC — see [`Service::html`].
fn html_script(id: u64) -> String {
    format!(
        r#"(function(){{
            var __id = {id};
            try {{
                window.ipc.postMessage(JSON.stringify({{id: __id, ok: true, html: document.documentElement.outerHTML}}));
            }} catch (e) {{
                window.ipc.postMessage(JSON.stringify({{id: __id, ok: false, error: String(e)}}));
            }}
        }})();"#
    )
}

/// JS that `fetch`es `url` from inside the page — so with the page's own
/// cookies, and for a `blob:` URL the page itself created — and reports
/// its body back over IPC, base64-encoded (see [`parse_fetched`]). The
/// download's bytes never touch the disk this way, unlike the engine's
/// native download machinery (see [`DriverHandler::on_download`]).
fn fetch_script(id: u64, url: &str) -> String {
    let url_json = json_literal(url);
    format!(
        r#"(function(){{
            var __id = {id};
            fetch({url_json}, {{credentials: "include"}}).then(function(response){{
                if (!response.ok) {{
                    throw new Error("download failed with HTTP status " + response.status);
                }}
                return response.arrayBuffer();
            }}).then(function(buffer){{
                if (buffer.byteLength > {MAX_BODY_BYTES}) {{
                    throw new Error("download is larger than the {MAX_BODY_BYTES} byte limit");
                }}
                // `String.fromCharCode.apply` over the whole buffer would
                // overflow the call stack on a large file.
                var bytes = new Uint8Array(buffer), chunks = [];
                for (var i = 0; i < bytes.length; i += 0x8000) {{
                    chunks.push(String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000)));
                }}
                window.ipc.postMessage(JSON.stringify({{id: __id, ok: true, data: btoa(chunks.join(""))}}));
            }}).catch(function(e){{
                window.ipc.postMessage(JSON.stringify({{id: __id, ok: false, error: String(e)}}));
            }});
        }})();"#
    )
}

/// Checks a download's `url` before [`fetch_script`] fetches it: the same
/// rule as [`is_navigation_allowed`], plus `data:` URLs, which carry their
/// content inline and never touch the network.
fn is_download_allowed(url: &str, allowed_domains: &[String]) -> bool {
    url.starts_with("data:") || is_navigation_allowed(url, allowed_domains)
}

/// Checks a navigation/new-window request's target `url` against
/// `allowed_domains`, the same way (and using the same rule — exact host or
/// subdomain match, empty list allowing nothing) `host::browser`'s
/// `fyde.browser:open` guard does. An unparseable `url` is rejected rather
/// than allowed — there's no host to check it against, so there's nothing
/// to justify letting it through.
///
/// `about:blank`/`about:srcdoc` are the one hostless exception: pages
/// routinely create such frames (e.g. Cloudflare Turnstile's widget does
/// several), they never touch the network, and blocking them silently
/// breaks whatever the page built them for.
///
/// A `data:` URL is hostless too, and carries its content inline without
/// touching the network: a page that downloads a file it fetched itself
/// clicks a `<a href="data:application/pdf;base64,..." download>` it built,
/// which WebKit runs through this handler before raising the download (see
/// [`is_download_allowed`]) — e.g. espace-client.orange.fr's contract
/// documents. Blocking it would silently cancel that download.
///
/// A `blob:` URL has no host of its own either, but embeds the origin of the
/// page that created it (`blob:https://www.example.com/<uuid>`), so it's
/// checked against that origin's host instead — e.g. bouyguestelecom.fr's
/// invoice links fetch the PDF with a bearer token and `window.open` the
/// resulting blob.
fn is_navigation_allowed(url: &str, allowed_domains: &[String]) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() == "about" {
        return matches!(parsed.path(), "blank" | "srcdoc");
    }
    if parsed.scheme() == "data" {
        return true;
    }
    if parsed.scheme() == "blob" {
        return match parsed.origin() {
            url::Origin::Tuple(_, host, _) => is_host_allowed(&host.to_string(), allowed_domains),
            url::Origin::Opaque(_) => false,
        };
    }
    parsed
        .host_str()
        .is_some_and(|host| is_host_allowed(host, allowed_domains))
}

/// Converts a saved [`Cookie`] (whether `fyde.http` or a previous run's
/// webview captured it) into the [`WebviewCookie`] an engine sets.
///
/// Every saved cookie is restored as a *domain* cookie on its saved domain
/// (its `Domain=` attribute if any, its origin's host otherwise), never as a
/// host-only one. Engines can't report which of the two a cookie they hold
/// is (see [`from_webview_cookie`]), so a site's `Domain=.example.com`
/// session cookie gets saved as plain `example.com`, and restoring that
/// host-only would never send it to `www.example.com`, silently logging the
/// next run out (confirmed on bouyguestelecom.fr, whose SSO and CAS cookies
/// are all domain cookies). Widening an originally host-only cookie to its
/// subdomains is the lesser evil: they're all inside `allowed_domains`
/// anyway.
fn to_webview_cookie(saved: &Cookie) -> Option<WebviewCookie> {
    let host = url::Url::parse(&saved.origin).ok()?.host_str()?.to_string();
    let cookie = cookie::Cookie::parse(saved.set_cookie.clone()).ok()?;
    let domain = cookie
        .domain()
        .unwrap_or(&host)
        .trim_start_matches('.')
        .to_string();
    let expires = match cookie.max_age() {
        Some(max_age) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|since| since.as_secs() as i64)
                .unwrap_or_default();
            Some(now.saturating_add(max_age.whole_seconds()))
        }
        None => cookie.expires_datetime().map(|at| at.unix_timestamp()),
    };
    Some(WebviewCookie {
        name: cookie.name().to_string(),
        value: cookie.value().to_string(),
        domain,
        path: cookie.path().unwrap_or("/").to_string(),
        secure: cookie.secure() == Some(true),
        http_only: cookie.http_only() == Some(true),
        same_site: cookie.same_site().map(|same_site| match same_site {
            cookie::SameSite::Strict => WebviewSameSite::Strict,
            cookie::SameSite::Lax => WebviewSameSite::Lax,
            cookie::SameSite::None => WebviewSameSite::None,
        }),
        expires,
    })
}

/// Converts one of the webview's own cookies into a [`Cookie`] ready to be
/// persisted alongside `fyde.http`'s, keyed by its domain only: whether it
/// was a domain or host-only cookie is lost on the way out of the engine
/// (see [`to_webview_cookie`], which restores it as a domain cookie), and so
/// is its expiry. Cookies for a host outside `allowed_domains` (left behind
/// by some third-party frame) are dropped: nothing in this run could ever
/// have used them.
fn from_webview_cookie(cookie: &WebviewCookie, allowed_domains: &[String]) -> Option<Cookie> {
    let domain = cookie.domain.trim_start_matches('.');
    if domain.is_empty() || !is_host_allowed(domain, allowed_domains) {
        return None;
    }
    let mut set_cookie = format!("{}={}; Path={}", cookie.name, cookie.value, cookie.path);
    if cookie.secure {
        set_cookie.push_str("; Secure");
    }
    if cookie.http_only {
        set_cookie.push_str("; HttpOnly");
    }
    Some(Cookie {
        origin: format!("https://{domain}"),
        set_cookie,
    })
}

/// A JS string literal safely encoding `value` — `serde_json`'s string
/// encoding happens to produce valid JS too (JSON string syntax is a subset
/// of JS string syntax).
fn json_literal(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn error_message(value: &Value) -> String {
    value
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown browser error")
        .to_string()
}

fn parse_ack(value: Value) -> Result<()> {
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(())
    } else {
        Err(ErrorKind::Browser(error_message(&value)).into())
    }
}

fn parse_html(value: Value) -> Result<String> {
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(ErrorKind::Browser(error_message(&value)).into());
    }
    Ok(value
        .get("html")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

fn parse_fetched(value: Value) -> Result<Vec<u8>> {
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(ErrorKind::Browser(error_message(&value)).into());
    }
    let data = value
        .get("data")
        .and_then(Value::as_str)
        .unwrap_or_default();
    // Checked here too: the page's own JS could post a reply that skips
    // `fetch_script`'s check.
    if base64::decoded_len_estimate(data.len()) > MAX_BODY_BYTES {
        return Err(ErrorKind::Browser(format!(
            "download is larger than the {MAX_BODY_BYTES} byte limit"
        ))
        .into());
    }
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|err| ErrorKind::Browser(format!("invalid downloaded data: {err}")).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::scrapers::browser::engine::{MockWebviewEngine, MockWebviewSession};
    use crate::domains::scrapers::cookies::FakeCookie;

    #[test]
    fn is_navigation_allowed_accepts_an_allowed_host_and_its_subdomains() {
        let allowed = vec!["example.com".to_string()];
        assert!(is_navigation_allowed("https://example.com/login", &allowed));
        assert!(is_navigation_allowed(
            "https://sub.example.com/login",
            &allowed
        ));
    }

    #[test]
    fn is_navigation_allowed_rejects_an_unrelated_host() {
        let allowed = vec!["example.com".to_string()];
        assert!(!is_navigation_allowed("https://evil.example/", &allowed));
    }

    #[test]
    fn is_navigation_allowed_rejects_an_unparseable_url() {
        let allowed = vec!["example.com".to_string()];
        assert!(!is_navigation_allowed("not a url", &allowed));
    }

    #[test]
    fn is_navigation_allowed_allows_nothing_when_the_list_is_empty() {
        assert!(!is_navigation_allowed("https://anything.example/", &[]));
    }

    #[test]
    fn to_webview_cookie_scopes_a_cookie_without_domain_to_its_origin_hosts_domain() {
        let saved = Cookie {
            origin: "https://www.example.com".to_string(),
            set_cookie: "session=abc".to_string(),
        };

        let cookie = to_webview_cookie(&saved).unwrap();

        assert_eq!(cookie.name, "session");
        assert_eq!(cookie.value, "abc");
        assert_eq!(cookie.domain, "www.example.com");
        assert_eq!(cookie.path, "/");
        assert_eq!(cookie.expires, None);
    }

    #[test]
    fn to_webview_cookie_keeps_an_explicit_domain_path_and_attributes() {
        let saved = Cookie {
            origin: "https://www.example.com".to_string(),
            set_cookie: "session=abc; Domain=.example.com; Path=/app; Secure; HttpOnly; \
                         SameSite=Strict; Expires=Wed, 21 Oct 2037 07:28:00 GMT"
                .to_string(),
        };

        let cookie = to_webview_cookie(&saved).unwrap();

        assert_eq!(cookie.domain, "example.com");
        assert_eq!(cookie.path, "/app");
        assert!(cookie.secure);
        assert!(cookie.http_only);
        assert_eq!(cookie.same_site, Some(WebviewSameSite::Strict));
        assert_eq!(cookie.expires, Some(2_139_722_880));
    }

    #[test]
    fn to_webview_cookie_turns_max_age_into_an_expiry_from_now() {
        let saved = Cookie {
            origin: "https://example.com".to_string(),
            set_cookie: "session=abc; Max-Age=3600".to_string(),
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let expires = to_webview_cookie(&saved).unwrap().expires.unwrap();

        assert!((now + 3590..=now + 3610).contains(&expires), "{expires}");
    }

    #[test]
    fn to_webview_cookie_rejects_an_unparseable_origin() {
        let saved = Cookie {
            origin: "not a url".to_string(),
            set_cookie: "session=abc".to_string(),
        };

        assert!(to_webview_cookie(&saved).is_none());
    }

    fn webview_cookie(name: &str, domain: &str) -> WebviewCookie {
        WebviewCookie {
            name: name.to_string(),
            value: "abc".to_string(),
            domain: domain.to_string(),
            path: "/".to_string(),
            secure: true,
            http_only: true,
            same_site: None,
            expires: Some(1_900_000_000),
        }
    }

    #[test]
    fn from_webview_cookie_formats_an_allowed_cookie_as_host_only() {
        let saved = from_webview_cookie(
            &webview_cookie("session", "www.example.com"),
            &["example.com".to_string()],
        )
        .unwrap();

        assert_eq!(saved.origin, "https://www.example.com");
        assert_eq!(saved.set_cookie, "session=abc; Path=/; Secure; HttpOnly");
    }

    #[test]
    fn from_webview_cookie_drops_a_cookie_for_a_disallowed_host() {
        assert!(
            from_webview_cookie(
                &webview_cookie("tracker", "evil.example"),
                &["example.com".to_string()]
            )
            .is_none()
        );
    }

    #[test]
    fn webview_cookie_round_trips_through_a_saved_cookie() {
        let original = Cookie {
            origin: "https://example.com".to_string(),
            set_cookie: "session=abc; Path=/; Secure".to_string(),
        };

        let restored = from_webview_cookie(
            &to_webview_cookie(&original).unwrap(),
            &["example.com".to_string()],
        )
        .unwrap();

        assert_eq!(restored, original);
    }

    #[test]
    fn is_navigation_allowed_lets_a_data_url_through() {
        let allowed = vec!["example.com".to_string()];
        assert!(is_navigation_allowed(
            "data:application/pdf;base64,JVBERi0=",
            &allowed
        ));
    }

    #[test]
    fn is_navigation_allowed_checks_a_blob_url_against_its_embedded_origin() {
        let allowed = vec!["example.com".to_string()];
        assert!(is_navigation_allowed(
            "blob:https://www.example.com/0b1d5a3e-8c1f-4a5e-9a57-2f9c3d1e7b6a",
            &allowed
        ));
        assert!(!is_navigation_allowed(
            "blob:https://evil.example/0b1d5a3e-8c1f-4a5e-9a57-2f9c3d1e7b6a",
            &allowed
        ));
        assert!(!is_navigation_allowed("blob:null/0b1d5a3e", &allowed));
    }

    #[test]
    fn is_navigation_allowed_accepts_about_blank_and_srcdoc_frames() {
        let allowed = vec!["example.com".to_string()];
        assert!(is_navigation_allowed("about:blank", &allowed));
        assert!(is_navigation_allowed("about:srcdoc", &allowed));
        assert!(!is_navigation_allowed("about:config", &allowed));
    }

    #[test]
    fn json_literal_escapes_quotes_and_special_characters() {
        assert_eq!(json_literal(r#"a"b"#), r#""a\"b""#);
        assert_eq!(json_literal("a\nb"), "\"a\\nb\"");
    }

    #[test]
    fn parse_ack_succeeds_on_an_ok_reply() {
        assert!(parse_ack(serde_json::json!({"id": 1, "ok": true})).is_ok());
    }

    #[test]
    fn parse_ack_fails_with_the_reported_error_message() {
        let err = parse_ack(serde_json::json!({"id": 1, "ok": false, "error": "no such element"}))
            .unwrap_err();
        assert_eq!(err.to_string(), "browser error: no such element");
    }

    #[test]
    fn parse_html_extracts_the_markup_on_success() {
        let html = parse_html(serde_json::json!({
            "id": 1,
            "ok": true,
            "html": "<html><body>hi</body></html>",
        }))
        .unwrap();

        assert_eq!(html, "<html><body>hi</body></html>");
    }

    #[test]
    fn parse_fetched_decodes_the_downloaded_bytes() {
        let bytes =
            parse_fetched(serde_json::json!({"id": 1, "ok": true, "data": "JVBERi0="})).unwrap();

        assert_eq!(bytes, b"%PDF-");
    }

    #[test]
    fn parse_fetched_fails_with_the_reported_error_message() {
        let err = parse_fetched(serde_json::json!({
            "id": 1,
            "ok": false,
            "error": "download failed with HTTP status 403"
        }))
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            "browser error: download failed with HTTP status 403"
        );
    }

    #[test]
    fn is_download_allowed_accepts_data_urls_and_allowed_hosts_only() {
        let allowed = vec!["example.com".to_string()];
        assert!(is_download_allowed(
            "data:application/pdf;base64,JVBERi0=",
            &allowed
        ));
        assert!(is_download_allowed(
            "https://www.example.com/doc.pdf",
            &allowed
        ));
        assert!(!is_download_allowed(
            "https://evil.example/doc.pdf",
            &allowed
        ));
    }

    #[test]
    fn parse_html_fails_with_the_reported_error_message() {
        let err = parse_html(serde_json::json!({
            "id": 1,
            "ok": false,
            "error": "no document"
        }))
        .unwrap_err();
        assert_eq!(err.to_string(), "browser error: no document");
    }

    /// The handler the driver gave its (mocked) engine, once it opened a
    /// session.
    type Captured = Arc<Mutex<Option<Arc<dyn WebviewHandler>>>>;

    fn handler_of(captured: &Captured) -> Arc<dyn WebviewHandler> {
        captured.lock().unwrap().clone().expect("no session opened")
    }

    /// An engine expected to open `session` exactly once.
    fn engine_opening(session: MockWebviewSession, captured: &Captured) -> MockWebviewEngine {
        let slot = captured.clone();
        let mut session = Some(session);
        let mut engine = MockWebviewEngine::new();
        engine
            .expect_open_session()
            .times(1)
            .returning(move |_config, handler| {
                *slot.lock().unwrap() = Some(handler);
                Ok(Box::new(session.take().unwrap()))
            });
        engine
    }

    /// The id a `*_script` snippet replies with.
    fn script_id(script: &str) -> u64 {
        let start = script.find("var __id = ").unwrap() + "var __id = ".len();
        script[start..].split(';').next().unwrap().parse().unwrap()
    }

    /// A session expected to be closed once, which answers every script it
    /// evaluates with what `answer` returns (if anything), tagged with the
    /// script's id, the way the page's `window.ipc.postMessage` would.
    fn answering_session(
        captured: &Captured,
        answer: impl Fn(&str, &dyn WebviewHandler) -> Option<Value> + Send + 'static,
    ) -> MockWebviewSession {
        let captured = captured.clone();
        let mut session = MockWebviewSession::new();
        session.expect_evaluate_script().returning(move |script| {
            let handler = handler_of(&captured);
            if let Some(mut reply) = answer(script, handler.as_ref()) {
                reply["id"] = script_id(script).into();
                handler.on_message(reply.to_string());
            }
            Ok(())
        });
        session.expect_close().times(1).return_const(());
        session
    }

    fn driver(engine: MockWebviewEngine) -> BrowserDriver {
        BrowserDriver::new(
            Arc::new(engine),
            Arc::new(vec!["example.com".to_string()]),
            true,
            vec![
                FakeCookie::new()
                    .with_origin("https://example.com")
                    .with_set_cookie("session=saved; Path=/")
                    .build(),
            ],
        )
    }

    #[tokio::test]
    async fn cookies_never_opens_a_webview_for_a_run_that_used_none() {
        let driver = driver(MockWebviewEngine::new());

        assert!(driver.cookies().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn open_opens_one_session_with_the_saved_cookies_then_loads_each_url() {
        let mut session = MockWebviewSession::new();
        session
            .expect_load_url()
            .withf(|url| url == "https://example.com/")
            .times(2)
            .returning(|_| Ok(()));
        session.expect_close().times(1).return_const(());
        let mut engine = MockWebviewEngine::new();
        engine
            .expect_open_session()
            .withf(|config, _| {
                config.visible
                    && config.cookies.len() == 1
                    && config.cookies[0].name == "session"
                    && config.cookies[0].value == "saved"
                    && config.cookies[0].domain == "example.com"
            })
            .times(1)
            .return_once(|_, _| Ok(Box::new(session)));
        let driver = driver(engine);

        driver.open("https://example.com/").await.unwrap();
        driver.open("https://example.com/").await.unwrap();
    }

    #[tokio::test]
    async fn a_failing_engine_fails_the_call_and_is_retried_by_the_next() {
        let mut engine = MockWebviewEngine::new();
        engine
            .expect_open_session()
            .times(2)
            .returning(|_, _| Err(ErrorKind::Browser("no display".to_string()).into()));
        let driver = driver(engine);

        let err = driver.open("https://example.com/").await.unwrap_err();
        assert_eq!(err.to_string(), "browser error: no display");
        assert!(driver.html().await.is_err());
    }

    #[tokio::test]
    async fn wait_for_and_fill_report_the_pages_reply() {
        let captured = Captured::default();
        let session = answering_session(&captured, |script, _| {
            Some(if script.contains("querySelector(\"#ok\")") {
                serde_json::json!({"ok": true})
            } else {
                serde_json::json!({"ok": false, "error": "element not found"})
            })
        });
        let driver = driver(engine_opening(session, &captured));

        driver
            .wait_for("#ok", Duration::from_secs(1))
            .await
            .unwrap();
        let err = driver.fill("#missing", "value").await.unwrap_err();
        assert_eq!(err.to_string(), "browser error: element not found");
    }

    #[tokio::test]
    async fn a_reply_with_another_id_is_ignored() {
        let captured = Captured::default();
        let session = answering_session(&captured, |_, handler| {
            handler.on_message(r#"{"id": 1, "ok": true, "html": "forged"}"#.to_string());
            None
        });
        let driver = driver(engine_opening(session, &captured));

        let err = driver
            .wait_for("body", Duration::from_millis(10))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("timed out"), "{err}");
    }

    #[tokio::test]
    async fn a_wait_given_up_on_stops_waiting_at_once_and_frees_its_slot() {
        let captured = Captured::default();
        // Never answers: the page would take the whole timeout.
        let session = answering_session(&captured, |_, _| None);
        let driver = driver(engine_opening(session, &captured));

        let started = Instant::now();
        let given_up = tokio::time::timeout(
            Duration::from_millis(50),
            driver.wait_for("body", Duration::from_secs(60)),
        )
        .await;

        assert!(given_up.is_err(), "the wait should still have been pending");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(driver.handler.pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_download_given_up_on_leaves_later_popups_alone() {
        let captured = Captured::default();
        let session = answering_session(&captured, |_, _| Some(serde_json::json!({"ok": true})));
        let driver = driver(engine_opening(session, &captured));

        let given_up = tokio::time::timeout(
            Duration::from_millis(50),
            driver.download("#pdf", Duration::from_secs(60)),
        )
        .await;

        assert!(
            given_up.is_err(),
            "the download should still have been pending"
        );
        assert!(handler_of(&captured).allow_new_window("https://www.example.com/popup"));
    }

    #[tokio::test]
    async fn a_window_closed_by_the_user_fails_the_waiting_call_and_every_later_one() {
        let captured = Captured::default();
        let session = answering_session(&captured, |_, handler| {
            handler.on_closed();
            None
        });
        let driver = driver(engine_opening(session, &captured));

        let err = driver.html().await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "browser error: the browser window was closed"
        );
        let err = driver.open("https://example.com/").await.unwrap_err();
        assert_eq!(
            err.to_string(),
            "browser error: the browser window was closed"
        );
    }

    #[tokio::test]
    async fn navigations_and_popups_outside_allowed_domains_are_refused() {
        let captured = Captured::default();
        let mut session = MockWebviewSession::new();
        session.expect_load_url().returning(|_| Ok(()));
        session.expect_close().times(1).return_const(());
        let driver = driver(engine_opening(session, &captured));
        driver.open("https://example.com/").await.unwrap();
        let handler = handler_of(&captured);

        assert!(handler.allow_navigation("https://www.example.com/login"));
        assert!(!handler.allow_navigation("https://evil.example/"));
        assert!(handler.allow_new_window("https://www.example.com/popup"));
        assert!(!handler.allow_new_window("https://evil.example/popup"));
    }

    #[tokio::test]
    async fn download_fetches_what_the_click_started_from_inside_the_page() {
        let captured = Captured::default();
        let session = answering_session(&captured, |script, handler| {
            if script.contains("el.click()") {
                handler.on_download("https://www.example.com/doc.pdf");
                Some(serde_json::json!({"ok": true}))
            } else {
                assert!(script.contains(r#"fetch("https://www.example.com/doc.pdf""#));
                Some(serde_json::json!({"ok": true, "data": "JVBERi0="}))
            }
        });
        let driver = driver(engine_opening(session, &captured));

        let bytes = driver
            .download("#pdf", Duration::from_secs(1))
            .await
            .unwrap();

        assert_eq!(bytes, b"%PDF-");
    }

    #[tokio::test]
    async fn download_takes_the_popup_the_click_opened_instead_of_opening_it() {
        let captured = Captured::default();
        let session = answering_session(&captured, |script, handler| {
            if script.contains("el.click()") {
                assert!(!handler.allow_new_window("https://www.example.com/doc.pdf"));
                Some(serde_json::json!({"ok": true}))
            } else {
                Some(serde_json::json!({"ok": true, "data": "JVBERi0="}))
            }
        });
        let driver = driver(engine_opening(session, &captured));

        let bytes = driver
            .download("#pdf", Duration::from_secs(1))
            .await
            .unwrap();

        assert_eq!(bytes, b"%PDF-");
    }

    #[tokio::test]
    async fn download_refuses_a_url_outside_allowed_domains_without_fetching_it() {
        let captured = Captured::default();
        let session = answering_session(&captured, |script, handler| {
            assert!(script.contains("el.click()"), "fetched a refused download");
            handler.on_download("https://evil.example/doc.pdf");
            Some(serde_json::json!({"ok": true}))
        });
        let driver = driver(engine_opening(session, &captured));

        let err = driver
            .download("#pdf", Duration::from_secs(1))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("allowed_domains"), "{err}");
    }

    #[tokio::test]
    async fn cookies_reads_back_only_the_allowed_hosts_cookies() {
        let captured = Captured::default();
        let mut session = MockWebviewSession::new();
        session.expect_load_url().returning(|_| Ok(()));
        session.expect_cookies().times(1).returning(|| {
            Ok(vec![
                webview_cookie("session", "www.example.com"),
                webview_cookie("tracker", "evil.example"),
            ])
        });
        session.expect_close().times(1).return_const(());
        let driver = driver(engine_opening(session, &captured));
        driver.open("https://example.com/").await.unwrap();

        let cookies = driver.cookies().await.unwrap();

        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].origin, "https://www.example.com");
    }
}
