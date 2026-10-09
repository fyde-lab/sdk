use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::run_return::EventLoopExtRunReturn as _;
use tao::window::WindowBuilder;
use uuid::Uuid;
use wry::WebViewBuilder;

use crate::{Error, Result};

use super::super::cookies::Cookie;
use super::super::host::is_host_allowed;
use super::Service;

/// One pending `Eval` call's reply slot, keyed by the id embedded in the JS
/// it sent — see [`BrowserDriver::call`]. Shared between the calling thread
/// (which inserts, then blocks on the receiving half) and the browser
/// thread's IPC handler (which looks the id up and fills it in once the
/// page posts a matching message back).
type PendingReplies = Arc<Mutex<HashMap<u64, SyncSender<Value>>>>;

/// The single in-flight download's reply slot, if any — set by
/// [`BrowserDriver::download`] right before it triggers the click that
/// starts the download, and filled in by the `download_completed_handler`
/// installed in [`run_event_loop`] once the webview engine reports the
/// download finished (or failed). `None` here (checked by the handler) means
/// no script-level `download` call is currently waiting, which can happen if
/// a page starts a download on its own outside of `download`'s control —
/// that outcome is logged and the file cleaned up rather than delivered
/// nowhere. Only one slot, not a map keyed by id like [`PendingReplies`],
/// since nothing here runs more than one download at a time (see
/// [`Service::download`]'s own doc comment).
type PendingDownload = Arc<Mutex<Option<SyncSender<Option<PathBuf>>>>>;

/// Sent from a [`BrowserDriver`] method to the dedicated OS thread actually
/// holding the `tao` event loop and `wry` webview — neither type is `Send`,
/// so every real interaction with them has to happen as a message over
/// this channel (via `EventLoopProxy::send_event`) rather than a direct
/// method call from the caller's own thread.
enum Command {
    /// Starts loading a URL — fire-and-forget, see [`Service::open`].
    Open(String),
    /// Runs a JS snippet that eventually calls
    /// `window.ipc.postMessage(JSON.stringify({id, ...}))`, whose `id`
    /// `call` is already waiting on in `PendingReplies`.
    Eval(String),
    /// Reads back every cookie the webview holds (see [`Service::cookies`]),
    /// already converted and filtered by [`from_webview_cookie`].
    Cookies(SyncSender<std::result::Result<Vec<Cookie>, String>>),
    /// Exits the event loop, ending the thread.
    Shutdown,
}

struct Running {
    proxy: EventLoopProxy<Command>,
    pending: PendingReplies,
    pending_download: PendingDownload,
    thread: Option<JoinHandle<()>>,
}

/// The real [`Service`] implementation: a webview embedded in an invisible
/// `tao` window, running entirely on one dedicated OS thread spawned on
/// first use (see [`ensure_started`](Self::ensure_started)). Every
/// `open`/`wait_for`/`fill`/`click` call is translated into a [`Command`]
/// sent over an [`EventLoopProxy`] and, for everything but `open`, a JS
/// snippet carrying a unique id that the webview's own `window.ipc`
/// eventually echoes back — see [`call`](Self::call).
pub(super) struct BrowserDriver {
    running: Mutex<Option<Running>>,
    next_id: AtomicU64,
    allowed_domains: Arc<Vec<String>>,
    visible: bool,
    initial_cookies: Vec<Cookie>,
}

impl BrowserDriver {
    pub(super) fn new(
        allowed_domains: Arc<Vec<String>>,
        visible: bool,
        initial_cookies: Vec<Cookie>,
    ) -> Self {
        Self {
            running: Mutex::new(None),
            next_id: AtomicU64::new(1),
            allowed_domains,
            visible,
            initial_cookies,
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Spawns the dedicated OS thread owning this run's `tao` event loop and
    /// `wry` webview, the first time any [`Service`] method is actually
    /// called — never at construction. A scraper script that only ever
    /// calls `fyde.http`/`fyde.html` (every scraper so far, except whatever
    /// newly adopts `fyde.browser`) never pays the cost, or the risk, of
    /// creating a real window: `wry`'s `WebView` needs a live window-system
    /// connection (X11/Wayland on Linux) that a headless server or CI
    /// runner may simply not have, and this way that's only ever a problem
    /// for a run that actually touches `fyde.browser`.
    fn ensure_started(&self) -> Result<(EventLoopProxy<Command>, PendingReplies, PendingDownload)> {
        let mut guard = self.running.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(running) = guard.as_ref() {
            return Ok((
                running.proxy.clone(),
                running.pending.clone(),
                running.pending_download.clone(),
            ));
        }

        let pending: PendingReplies = Arc::new(Mutex::new(HashMap::new()));
        let pending_for_thread = pending.clone();
        let pending_download: PendingDownload = Arc::new(Mutex::new(None));
        let pending_download_for_thread = pending_download.clone();

        // The `tao`/`wry` types involved (`EventLoop`, `WebView`, the GTK
        // objects underneath them on Linux) are all `!Send`, so none of
        // them can be built here and then moved into the spawned thread —
        // `run_event_loop` builds every one of them itself, from scratch,
        // on the thread that will actually run the event loop. Only the
        // resulting `EventLoopProxy` (designed from the start to be usable
        // from other threads) comes back out, over `ready_tx`.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<EventLoopProxy<Command>>>();

        let allowed_domains = self.allowed_domains.clone();
        let visible = self.visible;
        let initial_cookies = self.initial_cookies.clone();
        let thread = std::thread::Builder::new()
            .name("fyde-browser".to_string())
            .spawn(move || {
                run_event_loop(
                    pending_for_thread,
                    pending_download_for_thread,
                    ready_tx,
                    allowed_domains,
                    visible,
                    initial_cookies,
                )
            })
            .map_err(|err| Error::Browser(format!("failed to spawn browser thread: {err}")))?;

        // Block until the webview has actually been created (or failed to
        // be) before handing the proxy back, so a command sent right after
        // `ensure_started` returns can never race the webview's own setup.
        let proxy = ready_rx.recv().map_err(|_| {
            Error::Browser("browser thread exited before it finished starting up".to_string())
        })??;

        *guard = Some(Running {
            proxy: proxy.clone(),
            pending: pending.clone(),
            pending_download: pending_download.clone(),
            thread: Some(thread),
        });

        Ok((proxy, pending, pending_download))
    }

    /// Runs one `Eval` round-trip: allocates an id, registers its reply slot
    /// in `PendingReplies` *before* sending the command (so the IPC handler
    /// on the browser thread can never receive a reply for an id nothing is
    /// waiting on yet), builds the script via `build_js`, sends it, and
    /// blocks on the reply (or `timeout`). Used by `wait_for`/`fill`/
    /// `click`/`html` — `open` has no reply to wait for, so it bypasses
    /// this.
    fn call(&self, build_js: impl FnOnce(u64) -> String, timeout: Duration) -> Result<Value> {
        let (proxy, pending, _pending_download) = self.ensure_started()?;
        let id = self.next_id();

        let (tx, rx) = sync_channel(1);
        pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id, tx);

        let script = build_js(id);
        if proxy.send_event(Command::Eval(script)).is_err() {
            pending
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id);
            return Err(Error::Browser(
                "browser thread is no longer running".to_string(),
            ));
        }

        match rx.recv_timeout(timeout) {
            Ok(value) => Ok(value),
            Err(_) => {
                pending
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&id);
                Err(Error::Browser(format!(
                    "timed out after {timeout:?} waiting for the browser"
                )))
            }
        }
    }
}

impl Drop for BrowserDriver {
    fn drop(&mut self) {
        let running = self
            .running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(mut running) = running {
            let _ = running.proxy.send_event(Command::Shutdown);
            if let Some(thread) = running.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

#[async_trait]
impl Service for BrowserDriver {
    async fn open(&self, url: &str) -> Result<()> {
        let (proxy, _pending, _pending_download) = self.ensure_started()?;
        proxy
            .send_event(Command::Open(url.to_string()))
            .map_err(|_| Error::Browser("browser thread is no longer running".to_string()))
    }

    async fn wait_for(&self, selector: &str, timeout: Duration) -> Result<()> {
        let timeout_ms = timeout.as_millis() as u64;
        let value = self.call(
            |id| wait_for_script(id, selector, timeout_ms),
            timeout + Duration::from_secs(1),
        )?;
        parse_ack(value)
    }

    async fn fill(&self, selector: &str, value: &str) -> Result<()> {
        let result = self.call(
            |id| fill_script(id, selector, value),
            Duration::from_secs(10),
        )?;
        parse_ack(result)
    }

    async fn click(&self, selector: &str) -> Result<()> {
        let result = self.call(|id| click_script(id, selector), Duration::from_secs(10))?;
        parse_ack(result)
    }

    async fn html(&self) -> Result<String> {
        let value = self.call(html_script, Duration::from_secs(10))?;
        parse_html(value)
    }

    async fn cookies(&self) -> Result<Vec<Cookie>> {
        // Deliberately not `ensure_started`: a run that never touched
        // `fyde.browser` has no cookies here, and mustn't open a window
        // just to find that out.
        let proxy = match self
            .running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            Some(running) => running.proxy.clone(),
            None => return Ok(Vec::new()),
        };

        let (tx, rx) = sync_channel(1);
        proxy
            .send_event(Command::Cookies(tx))
            .map_err(|_| Error::Browser("browser thread is no longer running".to_string()))?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| Error::Browser("timed out reading the browser's cookies".to_string()))?
            .map_err(|err| Error::Browser(format!("reading the browser's cookies: {err}")))
    }

    async fn download(&self, selector: &str, timeout: Duration) -> Result<Vec<u8>> {
        let (_proxy, _pending, pending_download) = self.ensure_started()?;

        let (tx, rx) = sync_channel(1);
        *pending_download.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);

        // Reuses the same click path `Service::click` does — the download
        // is a side effect of this click, not a separate webview command.
        if let Err(err) = self.click(selector).await {
            *pending_download.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return Err(err);
        }

        let path = match rx.recv_timeout(timeout) {
            Ok(Some(path)) => path,
            Ok(None) => {
                return Err(Error::Browser(
                    "download failed or was cancelled by the browser engine".to_string(),
                ));
            }
            Err(_) => {
                *pending_download.lock().unwrap_or_else(|p| p.into_inner()) = None;
                return Err(Error::Browser(format!(
                    "timed out after {timeout:?} waiting for a download to complete"
                )));
            }
        };

        let bytes = std::fs::read(&path).map_err(|err| {
            Error::Browser(format!("failed to read downloaded file {path:?}: {err}"))
        })?;
        let _ = std::fs::remove_file(&path);
        Ok(bytes)
    }
}

/// Builds the `tao` event loop and `wry` webview and owns them for as long
/// as this run's [`BrowserDriver`] is alive, entirely on the thread
/// [`ensure_started`](BrowserDriver::ensure_started) spawned to call this —
/// `WebView`/`EventLoop` are both `!Send`, so neither can be built anywhere
/// else and moved in; only the resulting [`EventLoopProxy`] (sent back over
/// `ready_tx` once setup finishes, successfully or not) is designed to
/// cross threads. Processes [`Command`]s until `Command::Shutdown` (or the
/// window's own close button) exits the loop.
fn run_event_loop(
    pending: PendingReplies,
    pending_download: PendingDownload,
    ready_tx: Sender<Result<EventLoopProxy<Command>>>,
    allowed_domains: Arc<Vec<String>>,
    visible: bool,
    initial_cookies: Vec<Cookie>,
) {
    let mut builder = EventLoopBuilder::<Command>::with_user_event();
    #[cfg(target_os = "linux")]
    {
        use tao::platform::unix::EventLoopBuilderExtUnix as _;
        builder.with_any_thread(true);
    }
    let mut event_loop = builder.build();
    let proxy = event_loop.create_proxy();

    let mut window_builder = WindowBuilder::new().with_visible(visible);
    if visible {
        window_builder = window_builder
            .with_title("fyde scraper")
            .with_inner_size(tao::dpi::LogicalSize::new(1200.0, 900.0));
    }
    // tao packs a `gtk::Box` into the window by default (for its own GTK layout needs), which
    // leaves no room for `build_gtk` below to add the webview directly: `GtkApplicationWindow`
    // is a `GtkBin` subclass and can only ever hold one child, so the two conflict — observed
    // as a `Gtk-WARNING` and a webview that's constructed but never actually attached (so it
    // never loads/renders anything, and every `wait_for` call just times out). This is
    // structural (`GtkBin` only ever has room for one child), not about visibility, so it's
    // disabled unconditionally — wry's own docs for exactly this `build_gtk`-on-
    // `window.gtk_window()` pattern note the same conflict and recommend packing a `gtk::Fixed`
    // into that default box instead of disabling it, but there's nothing for a `gtk::Fixed`'s
    // layout behavior to do here since the webview is the window's only content either way.
    #[cfg(target_os = "linux")]
    {
        use tao::platform::unix::WindowBuilderExtUnix as _;
        window_builder = window_builder.with_default_vbox(false);
    }
    let window = match window_builder.build(&event_loop) {
        Ok(window) => window,
        Err(err) => {
            let _ = ready_tx.send(Err(Error::Browser(format!(
                "failed to create browser window: {err}"
            ))));
            return;
        }
    };

    let ipc_handler = move |request: wry::http::Request<String>| {
        let Ok(parsed) = serde_json::from_str::<Value>(request.body()) else {
            return;
        };
        let Some(id) = parsed.get("id").and_then(Value::as_u64) else {
            return;
        };
        let sender = pending
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        if let Some(sender) = sender {
            let _ = sender.send(parsed);
        }
    };

    let cookies_allowed_domains = allowed_domains.clone();
    let navigation_allowed_domains = allowed_domains.clone();
    let new_window_allowed_domains = allowed_domains;
    let builder = WebViewBuilder::new()
        .with_ipc_handler(ipc_handler)
        // Gates every top-level (and subframe) navigation the webview makes
        // on its own after `open`'s initial load — a link click, a form
        // submit that does a real navigation, a JS `location` change, a
        // server redirect — against the same `allowed_domains` list
        // `fyde.browser:open` itself is checked against (`host::browser`).
        // Doesn't see subresource loads (`fetch`/images/scripts/etc.),
        // which don't navigate anything; those aren't covered by this.
        .with_navigation_handler(move |url| {
            let allowed = is_navigation_allowed(&url, &navigation_allowed_domains);
            if !allowed {
                tracing::warn!(%url, "browser navigation blocked by allowed_domains");
            }
            allowed
        })
        // Same check for a `window.open(...)`/`target="_blank"` popup,
        // which `with_navigation_handler` doesn't see since it isn't a
        // navigation of the webview that requested it.
        .with_new_window_req_handler(move |url, _features| {
            if is_navigation_allowed(&url, &new_window_allowed_domains) {
                wry::NewWindowResponse::Allow
            } else {
                wry::NewWindowResponse::Deny
            }
        })
        // Redirects every download to a fresh, unique path under the OS
        // temp dir instead of the engine's default (a real downloads
        // directory, which would also prompt/collide across concurrent
        // runs) — `Service::download` reads this path back once the
        // completed handler below reports it finished, then deletes it.
        // Always allows the download (`true`): the resource it downloads
        // from was already reached through a page this webview navigated
        // to, which `with_navigation_handler`/`with_new_window_req_handler`
        // above already gated against `allowed_domains`.
        .with_download_started_handler(move |_url, path| {
            *path = std::env::temp_dir().join(format!("fyde-browser-download-{}", Uuid::new_v4()));
            true
        })
        // Delivers the finished (or failed) download to whichever
        // `Service::download` call is currently waiting, via
        // `pending_download` — see that type's own doc comment for why a
        // single slot, not a map, is enough here. A download that
        // completes with nobody waiting (nothing in this host triggers one
        // outside of `Service::download` itself, but a page could still do
        // it unprompted) is logged and its file cleaned up rather than
        // silently leaked on disk.
        .with_download_completed_handler(move |_url, path, success| {
            let waiting = pending_download
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take();
            match waiting {
                Some(sender) => {
                    let _ = sender.send(if success { path } else { None });
                }
                None => {
                    tracing::warn!(
                        "fyde.browser: a download completed (success={success}) with no \
                         pending `download` call waiting for it"
                    );
                    if let Some(path) = path {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
        });

    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix as _;
        use wry::WebViewBuilderExtUnix as _;
        builder.build_gtk(window.gtk_window())
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window);

    let webview = match webview {
        Ok(webview) => webview,
        Err(err) => {
            let _ = ready_tx.send(Err(Error::Browser(format!(
                "failed to create webview: {err}"
            ))));
            return;
        }
    };

    // Seeded before `ready_tx` fires, so no `open` can race ahead of them.
    for saved in &initial_cookies {
        let Some(cookie) = to_webview_cookie(saved) else {
            tracing::warn!(origin = %saved.origin, "fyde.browser: skipping an unparseable saved cookie");
            continue;
        };
        if let Err(err) = webview.set_cookie(&cookie) {
            tracing::warn!(origin = %saved.origin, "fyde.browser: failed to restore a saved cookie: {err}");
        }
    }

    if ready_tx.send(Ok(proxy)).is_err() {
        return;
    }

    event_loop.run_return(move |event, _target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(Command::Open(url)) => {
                if let Err(err) = webview.load_url(&url) {
                    tracing::error!("fyde.browser: load_url({url:?}) failed: {err}");
                }
            }
            Event::UserEvent(Command::Eval(script)) => {
                if let Err(err) = webview.evaluate_script(&script) {
                    tracing::error!("fyde.browser: evaluate_script failed: {err}");
                }
            }
            Event::UserEvent(Command::Cookies(reply)) => {
                let cookies = webview
                    .cookies()
                    .map(|cookies| {
                        cookies
                            .iter()
                            .filter_map(|cookie| {
                                from_webview_cookie(cookie, &cookies_allowed_domains)
                            })
                            .collect()
                    })
                    .map_err(|err| err.to_string());
                let _ = reply.send(cookies);
            }
            Event::UserEvent(Command::Shutdown) => {
                *control_flow = ControlFlow::Exit;
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
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
fn is_navigation_allowed(url: &str, allowed_domains: &[String]) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if parsed.scheme() == "about" {
        return matches!(parsed.path(), "blank" | "srcdoc");
    }
    parsed
        .host_str()
        .is_some_and(|host| is_host_allowed(host, allowed_domains))
}

/// Converts a saved [`Cookie`] (whether `fyde.http` or a previous run's
/// webview captured it) into the form `WebView::set_cookie` takes.
///
/// Every cookie crossing `wry`'s cookie API ends up host-only: `wry` hands
/// libsoup `cookie::Cookie::domain()`, which strips the leading dot that
/// would mark a domain cookie, so there is no way to express `Domain=` scope
/// through it. A saved `Domain=example.com` cookie is restored as host-only
/// on `example.com`; anything else as host-only on its origin's host.
fn to_webview_cookie(saved: &Cookie) -> Option<cookie::Cookie<'static>> {
    let host = url::Url::parse(&saved.origin).ok()?.host_str()?.to_string();
    let mut cookie = cookie::Cookie::parse(saved.set_cookie.clone()).ok()?;
    let domain = cookie.domain().map(str::to_string).unwrap_or(host);
    cookie.set_domain(domain);
    if cookie.path().is_none() {
        cookie.set_path("/");
    }
    Some(cookie)
}

/// Converts one of the webview's own cookies into a [`Cookie`] ready to be
/// persisted alongside `fyde.http`'s — host-only on its domain, for the
/// reason [`to_webview_cookie`] explains. Cookies for a host outside
/// `allowed_domains` (left behind by some third-party frame) are dropped:
/// nothing in this run could ever have used them.
fn from_webview_cookie(cookie: &cookie::Cookie<'_>, allowed_domains: &[String]) -> Option<Cookie> {
    let domain = cookie.domain()?;
    if !is_host_allowed(domain, allowed_domains) {
        return None;
    }
    let mut set_cookie = format!("{}={}", cookie.name(), cookie.value());
    if let Some(path) = cookie.path() {
        set_cookie.push_str(&format!("; Path={path}"));
    }
    if cookie.secure() == Some(true) {
        set_cookie.push_str("; Secure");
    }
    if cookie.http_only() == Some(true) {
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
        Err(Error::Browser(error_message(&value)))
    }
}

fn parse_html(value: Value) -> Result<String> {
    if value.get("ok").and_then(Value::as_bool) != Some(true) {
        return Err(Error::Browser(error_message(&value)));
    }
    Ok(value
        .get("html")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn to_webview_cookie_scopes_a_host_only_cookie_to_its_origin_host() {
        let saved = Cookie {
            origin: "https://www.example.com".to_string(),
            set_cookie: "session=abc".to_string(),
        };

        let cookie = to_webview_cookie(&saved).unwrap();

        assert_eq!(cookie.name(), "session");
        assert_eq!(cookie.value(), "abc");
        assert_eq!(cookie.domain(), Some("www.example.com"));
        assert_eq!(cookie.path(), Some("/"));
    }

    #[test]
    fn to_webview_cookie_keeps_an_explicit_domain_and_path() {
        let saved = Cookie {
            origin: "https://www.example.com".to_string(),
            set_cookie: "session=abc; Domain=example.com; Path=/app; Secure".to_string(),
        };

        let cookie = to_webview_cookie(&saved).unwrap();

        assert_eq!(cookie.domain(), Some("example.com"));
        assert_eq!(cookie.path(), Some("/app"));
        assert_eq!(cookie.secure(), Some(true));
    }

    #[test]
    fn to_webview_cookie_rejects_an_unparseable_origin() {
        let saved = Cookie {
            origin: "not a url".to_string(),
            set_cookie: "session=abc".to_string(),
        };

        assert!(to_webview_cookie(&saved).is_none());
    }

    #[test]
    fn from_webview_cookie_formats_an_allowed_cookie_as_host_only() {
        let cookie = cookie::Cookie::build(("session", "abc"))
            .domain("www.example.com")
            .path("/")
            .secure(true)
            .http_only(true)
            .build();

        let saved = from_webview_cookie(&cookie, &["example.com".to_string()]).unwrap();

        assert_eq!(saved.origin, "https://www.example.com");
        assert_eq!(saved.set_cookie, "session=abc; Path=/; Secure; HttpOnly");
    }

    #[test]
    fn from_webview_cookie_drops_a_cookie_for_a_disallowed_host() {
        let cookie = cookie::Cookie::build(("tracker", "1"))
            .domain("evil.example")
            .build();

        assert!(from_webview_cookie(&cookie, &["example.com".to_string()]).is_none());
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
    fn parse_html_fails_with_the_reported_error_message() {
        let err = parse_html(serde_json::json!({
            "id": 1,
            "ok": false,
            "error": "no document"
        }))
        .unwrap_err();
        assert_eq!(err.to_string(), "browser error: no document");
    }
}
