use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Sender, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine as _;
use serde_json::Value;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoop, EventLoopBuilder, EventLoopProxy};
use tao::platform::run_return::EventLoopExtRunReturn as _;
use tao::window::{Window, WindowBuilder};
use wry::{WebView, WebViewBuilder};

use crate::{ErrorKind, Result};

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
/// starts the download, and filled in with the download's URL by the
/// `download_started_handler` (or, for a popup, the
/// `new_window_req_handler`) installed in [`build_webview`]. `None` here
/// means no script-level `download` call is currently waiting, which can
/// happen if a page starts a download on its own outside of `download`'s
/// control — that download is just cancelled. Only one slot, not a map
/// keyed by id like [`PendingReplies`], since nothing here runs more than
/// one download at a time (see [`Service::download`]'s own doc comment).
type PendingDownload = Arc<Mutex<Option<SyncSender<String>>>>;

/// Hands `url` to the `download` call waiting in `pending_download`, if
/// any, emptying the slot.
fn deliver_download_url(pending_download: &PendingDownload, url: String) {
    let waiting = pending_download
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .take();
    match waiting {
        Some(sender) => {
            let _ = sender.send(url);
        }
        None => tracing::warn!(
            %url,
            "fyde.browser: cancelled a download no `download` call was waiting for"
        ),
    }
}

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

/// The real [`Service`] implementation: a webview embedded in a `tao`
/// window (hidden unless `visible`), running entirely on one dedicated OS
/// thread spawned on first use (see [`ensure_started`](Self::ensure_started)).
/// Every call is translated into a [`Command`] sent over an
/// [`EventLoopProxy`]; `wait_for`/`fill`/`click`/`html` send a JS snippet
/// carrying a unique id that the webview's own `window.ipc` eventually
/// echoes back — see [`call`](Self::call).
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
    /// calls `fyde.http`/`fyde.html` never pays the cost, or the risk, of
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
            .map_err(|err| ErrorKind::Browser(format!("failed to spawn browser thread: {err}")))?;

        // Block until the webview has actually been created (or failed to
        // be) before handing the proxy back, so a command sent right after
        // `ensure_started` returns can never race the webview's own setup.
        let proxy = ready_rx.recv().map_err(|_| {
            ErrorKind::Browser("browser thread exited before it finished starting up".to_string())
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
            return Err(
                ErrorKind::Browser("browser thread is no longer running".to_string()).into(),
            );
        }

        match rx.recv_timeout(timeout) {
            Ok(value) => Ok(value),
            Err(_) => {
                pending
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .remove(&id);
                Err(ErrorKind::Browser(format!(
                    "timed out after {timeout:?} waiting for the browser"
                ))
                .into())
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
            .map_err(|_| {
                ErrorKind::Browser("browser thread is no longer running".to_string()).into()
            })
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
            .map_err(|_| ErrorKind::Browser("browser thread is no longer running".to_string()))?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| ErrorKind::Browser("timed out reading the browser's cookies".to_string()))?
            .map_err(|err| {
                ErrorKind::Browser(format!("reading the browser's cookies: {err}")).into()
            })
    }

    async fn download(&self, selector: &str, timeout: Duration) -> Result<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let (_proxy, _pending, pending_download) = self.ensure_started()?;

        let (tx, rx) = sync_channel(1);
        *pending_download.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);

        // Reuses the same click path `Service::click` does — the download
        // is a side effect of this click, not a separate webview command.
        if let Err(err) = self.click(selector).await {
            *pending_download.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return Err(err);
        }

        let Ok(url) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) else {
            *pending_download.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return Err(ErrorKind::Browser(format!(
                "timed out after {timeout:?} waiting for a download to start"
            ))
            .into());
        };
        if !is_download_allowed(&url, &self.allowed_domains) {
            return Err(ErrorKind::Browser(format!(
                "download url {url:?} is not in this scraper's allowed_domains list"
            ))
            .into());
        }

        let value = self.call(
            |id| fetch_script(id, &url),
            deadline.saturating_duration_since(Instant::now()),
        )?;
        parse_fetched(value)
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

    let setup = build_window(&event_loop, visible).and_then(|window| {
        let webview = build_webview(&window, pending, pending_download, allowed_domains.clone())?;
        Ok((window, webview))
    });
    // `_window` (not `_`) keeps the window alive for as long as the loop
    // below runs: dropping it would close the webview's window under it.
    let (_window, webview) = match setup {
        Ok(setup) => setup,
        Err(err) => {
            let _ = ready_tx.send(Err(err));
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
            Event::UserEvent(command) => {
                handle_command(&webview, command, &allowed_domains, control_flow);
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

/// Builds the (hidden unless `visible`) window the webview attaches to.
fn build_window(event_loop: &EventLoop<Command>, visible: bool) -> Result<Window> {
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
    window_builder
        .build(event_loop)
        .map_err(|err| ErrorKind::Browser(format!("failed to create browser window: {err}")).into())
}

/// Builds the webview inside `window`: replies to [`PendingReplies`] over
/// IPC, gates navigations and popups against `allowed_domains`, and hands
/// download URLs to whichever `download` call [`PendingDownload`] holds.
fn build_webview(
    window: &Window,
    pending: PendingReplies,
    pending_download: PendingDownload,
    allowed_domains: Arc<Vec<String>>,
) -> Result<WebView> {
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

    let navigation_allowed_domains = allowed_domains.clone();
    let new_window_allowed_domains = allowed_domains;
    let builder = WebViewBuilder::new()
        // Ephemeral: without it wry uses the engine's default, persistent
        // data store (on Linux, `$XDG_DATA_HOME`/`$XDG_CACHE_HOME/<prgname>`),
        // leaving every site's localStorage, IndexedDB, service workers and
        // HTTP cache on disk outside the database `logout` wipes, shared
        // across runs, scrapers and accounts. The only state meant to
        // outlive a run is cookies, which `cookies` persists and `init`
        // seeds back in.
        .with_incognito(true)
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
        //
        // While a `Service::download` call is waiting, an allowed popup is
        // denied and its URL handed to that call instead: on Linux, wry's
        // `Allow` builds a separate, always-shown GTK window (ignoring
        // `visible`), so a page that opens its PDF in a popup (cesu's
        // `affichagePdf_<id>` buttons) would just display it there and
        // `download` would time out.
        .with_new_window_req_handler({
            let pending_download = pending_download.clone();
            move |url, _features| {
                if !is_navigation_allowed(&url, &new_window_allowed_domains) {
                    tracing::warn!(%url, "browser popup blocked by allowed_domains");
                    return wry::NewWindowResponse::Deny;
                }
                let download_waiting = pending_download
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .is_some();
                if download_waiting {
                    deliver_download_url(&pending_download, url);
                    return wry::NewWindowResponse::Deny;
                }
                wry::NewWindowResponse::Allow
            }
        })
        // Cancels every native download (`false`) before the engine writes
        // anything to disk, handing its URL to the waiting
        // `Service::download` call instead, which fetches the bytes from
        // inside the page (see `fetch_script`) so they only ever exist in
        // memory.
        .with_download_started_handler(move |url, _path| {
            deliver_download_url(&pending_download, url);
            false
        });

    #[cfg(target_os = "linux")]
    let webview = {
        use tao::platform::unix::WindowExtUnix as _;
        use wry::WebViewBuilderExtUnix as _;
        builder.build_gtk(window.gtk_window())
    };
    #[cfg(not(target_os = "linux"))]
    let webview = builder.build(&window);

    webview.map_err(|err| ErrorKind::Browser(format!("failed to create webview: {err}")).into())
}

/// Carries out one [`Command`] sent from [`BrowserDriver`] on the webview's
/// own thread.
fn handle_command(
    webview: &WebView,
    command: Command,
    allowed_domains: &[String],
    control_flow: &mut ControlFlow,
) {
    match command {
        Command::Open(url) => {
            if let Err(err) = webview.load_url(&url) {
                tracing::error!("fyde.browser: load_url({url:?}) failed: {err}");
            }
        }
        Command::Eval(script) => {
            if let Err(err) = webview.evaluate_script(&script) {
                tracing::error!("fyde.browser: evaluate_script failed: {err}");
            }
        }
        Command::Cookies(reply) => {
            let cookies = webview
                .cookies()
                .map(|cookies| {
                    cookies
                        .iter()
                        .filter_map(|cookie| from_webview_cookie(cookie, allowed_domains))
                        .collect()
                })
                .map_err(|err| err.to_string());
            let _ = reply.send(cookies);
        }
        Command::Shutdown => {
            *control_flow = ControlFlow::Exit;
        }
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
/// native download machinery (see `build_webview`'s
/// `with_download_started_handler`).
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
/// webview captured it) into the form `WebView::set_cookie` takes.
///
/// Every saved cookie is restored as a *domain* cookie on its saved domain
/// (its `Domain=` attribute if any, its origin's host otherwise), never as a
/// host-only one. Reading the webview's cookies back (see
/// [`from_webview_cookie`]) can't tell the two apart — `cookie::Cookie::domain()`
/// strips the leading dot libsoup uses to mark a domain cookie — so a site's
/// `Domain=.example.com` session cookie gets saved as plain `example.com`,
/// and restoring that host-only would never send it to `www.example.com`,
/// silently logging the next run out (confirmed on bouyguestelecom.fr, whose
/// SSO and CAS cookies are all domain cookies). Widening an originally
/// host-only cookie to its subdomains is the lesser evil: they're all inside
/// `allowed_domains` anyway.
///
/// `wry` hands libsoup `cookie.domain()`, which strips exactly one leading
/// dot — hence the double dot here, so libsoup receives `.example.com`.
fn to_webview_cookie(saved: &Cookie) -> Option<cookie::Cookie<'static>> {
    let host = url::Url::parse(&saved.origin).ok()?.host_str()?.to_string();
    let mut cookie = cookie::Cookie::parse(saved.set_cookie.clone()).ok()?;
    let domain = cookie.domain().map(str::to_string).unwrap_or(host);
    cookie.set_domain(format!("..{}", domain.trim_start_matches('.')));
    if cookie.path().is_none() {
        cookie.set_path("/");
    }
    Some(cookie)
}

/// Converts one of the webview's own cookies into a [`Cookie`] ready to be
/// persisted alongside `fyde.http`'s, keyed by its domain only: whether it
/// was a domain or host-only cookie is lost on the way out of `wry` (see
/// [`to_webview_cookie`], which restores it as a domain cookie). Cookies for a host outside
/// `allowed_domains` (left behind by some third-party frame) are dropped:
/// nothing in this run could ever have used them.
fn from_webview_cookie(cookie: &cookie::Cookie<'_>, allowed_domains: &[String]) -> Option<Cookie> {
    let domain = cookie.domain()?.trim_start_matches('.');
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
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|err| ErrorKind::Browser(format!("invalid downloaded data: {err}")).into())
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
    fn to_webview_cookie_scopes_a_cookie_without_domain_to_its_origin_hosts_domain() {
        let saved = Cookie {
            origin: "https://www.example.com".to_string(),
            set_cookie: "session=abc".to_string(),
        };

        let cookie = to_webview_cookie(&saved).unwrap();

        assert_eq!(cookie.name(), "session");
        assert_eq!(cookie.value(), "abc");
        assert_eq!(cookie.domain(), Some(".www.example.com"));
        assert_eq!(cookie.path(), Some("/"));
    }

    #[test]
    fn to_webview_cookie_keeps_an_explicit_domain_and_path() {
        let saved = Cookie {
            origin: "https://www.example.com".to_string(),
            set_cookie: "session=abc; Domain=example.com; Path=/app; Secure".to_string(),
        };

        let cookie = to_webview_cookie(&saved).unwrap();

        assert_eq!(cookie.domain(), Some(".example.com"));
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
}
