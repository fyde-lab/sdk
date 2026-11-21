//! The desktop [`WebviewEngine`]: `wry` webviews in `tao` windows, all on
//! one process-wide browser thread.

use std::collections::HashMap;
use std::sync::mpsc::{Sender, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::RngExt as _;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::run_return::EventLoopExtRunReturn as _;
use tao::window::{Window, WindowBuilder};
use wry::{WebView, WebViewBuilder};

use crate::{ErrorKind, Result};

use super::engine::{
    WebviewConfig, WebviewCookie, WebviewEngine, WebviewHandler, WebviewSameSite, WebviewSession,
};

/// Sent to the process-wide browser thread (see [`host_proxy`]) holding the
/// `tao` event loop and every session's `wry` webview — neither type is
/// `Send`, so every interaction with them is a message over this channel
/// (via `EventLoopProxy::send_event`) rather than a direct call from the
/// caller's thread. Every command but `Create` names the session it
/// targets, so concurrent runs never reach each other's webview.
enum Command {
    Create {
        session: u64,
        config: WebviewConfig,
        handler: Arc<dyn WebviewHandler>,
        ready: Sender<Result<()>>,
    },
    Open {
        session: u64,
        url: String,
    },
    Eval {
        session: u64,
        script: String,
    },
    Cookies {
        session: u64,
        reply: SyncSender<std::result::Result<Vec<WebviewCookie>, String>>,
    },
    Close(u64),
}

/// One session's window and webview, owned by the browser thread.
struct Session {
    // Kept alive for as long as the session is: dropping it would close the
    // webview's window under it.
    window: Window,
    webview: WebView,
    handler: Arc<dyn WebviewHandler>,
}

/// The proxy to the process-wide browser thread, once it's running. There
/// is only ever one: on Linux, GTK can only be used from the thread that
/// initialized it (`gtk::init` panics when called from a second one), so
/// one thread hosts the event loop and every session's webview, rather
/// than a thread per run.
static HOST: Mutex<Option<EventLoopProxy<Command>>> = Mutex::new(None);

/// Returns a proxy to the browser thread, spawning it the first time a
/// session is opened. If the thread failed to start (no display, say),
/// nothing is kept, so a later run tries again.
fn host_proxy() -> Result<EventLoopProxy<Command>> {
    let mut host = HOST.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(proxy) = host.as_ref() {
        return Ok(proxy.clone());
    }

    // The `tao`/`wry` types involved (`EventLoop`, `WebView`, the GTK
    // objects underneath them on Linux) are all `!Send`, so none of them
    // can be built here and then moved into the spawned thread —
    // `run_event_loop` builds every one of them itself, on the thread that
    // will actually run the event loop. Only the resulting `EventLoopProxy`
    // (designed to be usable from other threads) comes back out.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<EventLoopProxy<Command>>();
    std::thread::Builder::new()
        .name("fyde-browser".to_string())
        .spawn(move || run_event_loop(ready_tx))
        .map_err(|err| ErrorKind::Browser(format!("failed to spawn browser thread: {err}")))?;
    let proxy = ready_rx.recv().map_err(|_| {
        ErrorKind::Browser("browser thread exited before it finished starting up".to_string())
    })?;

    *host = Some(proxy.clone());
    Ok(proxy)
}

fn thread_gone() -> crate::Error {
    ErrorKind::Browser("browser thread is no longer running".to_string()).into()
}

/// `wry` webviews in `tao` windows (hidden unless
/// [`WebviewConfig::visible`]), on a browser thread started on the first
/// [`open_session`](WebviewEngine::open_session).
pub(crate) struct WryEngine;

impl WebviewEngine for WryEngine {
    fn open_session(
        &self,
        config: WebviewConfig,
        handler: Arc<dyn WebviewHandler>,
    ) -> Result<Box<dyn WebviewSession>> {
        let proxy = host_proxy()?;
        let session = rand::rng().random::<u64>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let create = Command::Create {
            session,
            config,
            handler,
            ready: ready_tx,
        };
        if proxy.send_event(create).is_err() {
            // The loop is gone; forget it so the next run starts a new one.
            HOST.lock().unwrap_or_else(|p| p.into_inner()).take();
            return Err(thread_gone());
        }

        // Block until the webview has actually been created (or failed to
        // be), so a command sent right after can never race its setup.
        ready_rx.recv().map_err(|_| {
            ErrorKind::Browser("browser thread exited before opening a window".to_string())
        })??;

        Ok(Box::new(WrySession {
            proxy: Mutex::new(proxy),
            session,
        }))
    }
}

/// A [`WryEngine`] session: commands for it go over the browser thread's
/// proxy, tagged with its id.
struct WrySession {
    // `EventLoopProxy` is `Send` but not `Sync`.
    proxy: Mutex<EventLoopProxy<Command>>,
    session: u64,
}

impl WrySession {
    fn send(&self, command: Command) -> Result<()> {
        self.proxy
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .send_event(command)
            .map_err(|_| thread_gone())
    }
}

impl WebviewSession for WrySession {
    fn load_url(&self, url: &str) -> Result<()> {
        self.send(Command::Open {
            session: self.session,
            url: url.to_string(),
        })
    }

    fn evaluate_script(&self, script: &str) -> Result<()> {
        self.send(Command::Eval {
            session: self.session,
            script: script.to_string(),
        })
    }

    fn cookies(&self) -> Result<Vec<WebviewCookie>> {
        let (tx, rx) = sync_channel(1);
        self.send(Command::Cookies {
            session: self.session,
            reply: tx,
        })?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| ErrorKind::Browser("timed out reading the browser's cookies".to_string()))?
            .map_err(|err| {
                ErrorKind::Browser(format!("reading the browser's cookies: {err}")).into()
            })
    }

    fn close(&self) {
        let _ = self.send(Command::Close(self.session));
    }
}

/// Builds the `tao` event loop on the process-wide browser thread
/// [`host_proxy`] spawned to call this, and runs it for the rest of the
/// process — `EventLoop`/`WebView` are both `!Send`, so neither can be
/// built anywhere else and moved in; only the resulting [`EventLoopProxy`]
/// (sent back over `ready_tx`) is designed to cross threads. Each
/// [`Session`] lives in `sessions` from its `Command::Create` until its
/// `Command::Close` (or its window's own close button).
fn run_event_loop(ready_tx: Sender<EventLoopProxy<Command>>) {
    let mut builder = EventLoopBuilder::<Command>::with_user_event();
    #[cfg(target_os = "linux")]
    {
        use tao::platform::unix::EventLoopBuilderExtUnix as _;
        builder.with_any_thread(true);
    }
    let mut event_loop = builder.build();
    if ready_tx.send(event_loop.create_proxy()).is_err() {
        return;
    }

    let mut sessions: HashMap<u64, Session> = HashMap::new();
    event_loop.run_return(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::UserEvent(command) => handle_command(&mut sessions, target, command),
            Event::WindowEvent {
                window_id,
                event: WindowEvent::CloseRequested,
                ..
            } => {
                let closed = sessions
                    .iter()
                    .find(|(_, session)| session.window.id() == window_id)
                    .map(|(id, _)| *id);
                if let Some(session) = closed.and_then(|id| sessions.remove(&id)) {
                    session.handler.on_closed();
                }
            }
            _ => {}
        }
    });
}

/// Opens one session's window and webview, seeded with its cookies.
fn open_session(
    target: &EventLoopWindowTarget<Command>,
    config: WebviewConfig,
    handler: Arc<dyn WebviewHandler>,
) -> Result<Session> {
    let window = build_window(target, config.visible)?;
    let webview = build_webview(&window, handler.clone())?;

    for saved in &config.cookies {
        if let Err(err) = webview.set_cookie(&to_wry_cookie(saved)) {
            tracing::warn!(
                domain = %saved.domain,
                "fyde.browser: failed to restore a saved cookie: {err}"
            );
        }
    }

    Ok(Session {
        window,
        webview,
        handler,
    })
}

/// Builds the (hidden unless `visible`) window the webview attaches to.
fn build_window(target: &EventLoopWindowTarget<Command>, visible: bool) -> Result<Window> {
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
        .build(target)
        .map_err(|err| ErrorKind::Browser(format!("failed to create browser window: {err}")).into())
}

/// Builds the webview inside `window`, every request of which `handler`
/// answers.
fn build_webview(window: &Window, handler: Arc<dyn WebviewHandler>) -> Result<WebView> {
    let builder = WebViewBuilder::new()
        // Ephemeral: without it wry uses the engine's default, persistent
        // data store (on Linux, `$XDG_DATA_HOME`/`$XDG_CACHE_HOME/<prgname>`),
        // leaving every site's localStorage, IndexedDB, service workers and
        // HTTP cache on disk outside the client directory `logout` wipes,
        // shared across runs, scrapers and accounts.
        .with_incognito(true)
        .with_ipc_handler({
            let handler = handler.clone();
            move |request: wry::http::Request<String>| handler.on_message(request.into_body())
        })
        // Every top-level (and subframe) navigation: a link click, a form
        // submit, a JS `location` change, a server redirect. Doesn't see
        // subresource loads (`fetch`/images/scripts/etc.), which don't
        // navigate anything.
        .with_navigation_handler({
            let handler = handler.clone();
            move |url| handler.allow_navigation(&url)
        })
        // A `window.open(...)`/`target="_blank"` popup, which the navigation
        // handler doesn't see since it isn't a navigation of this webview.
        // On Linux, `Allow` builds a separate, always-shown GTK window
        // (ignoring `visible`).
        .with_new_window_req_handler({
            let handler = handler.clone();
            move |url, _features| {
                if handler.allow_new_window(&url) {
                    wry::NewWindowResponse::Allow
                } else {
                    wry::NewWindowResponse::Deny
                }
            }
        })
        // Cancels every native download (`false`) before the engine writes
        // anything to disk.
        .with_download_started_handler(move |url, _path| {
            handler.on_download(&url);
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

/// Carries out one [`Command`], on the browser thread, against the
/// [`Session`] it names. A command for a session that no longer exists (its
/// window was closed) is dropped: its handler already heard.
fn handle_command(
    sessions: &mut HashMap<u64, Session>,
    target: &EventLoopWindowTarget<Command>,
    command: Command,
) {
    match command {
        Command::Create {
            session,
            config,
            handler,
            ready,
        } => match open_session(target, config, handler) {
            Ok(opened) => {
                sessions.insert(session, opened);
                let _ = ready.send(Ok(()));
            }
            Err(err) => {
                let _ = ready.send(Err(err));
            }
        },
        Command::Open { session, url } => {
            let Some(session) = sessions.get(&session) else {
                return;
            };
            if let Err(err) = session.webview.load_url(&url) {
                tracing::error!("fyde.browser: load_url failed: {err}");
            }
        }
        Command::Eval { session, script } => {
            let Some(session) = sessions.get(&session) else {
                return;
            };
            if let Err(err) = session.webview.evaluate_script(&script) {
                tracing::error!("fyde.browser: evaluate_script failed: {err}");
            }
        }
        Command::Cookies { session, reply } => {
            let Some(session) = sessions.get(&session) else {
                let _ = reply.send(Err("the browser window was closed".to_string()));
                return;
            };
            let cookies = session
                .webview
                .cookies()
                .map(|cookies| cookies.iter().filter_map(from_wry_cookie).collect())
                .map_err(|err| err.to_string());
            let _ = reply.send(cookies);
        }
        Command::Close(session) => {
            sessions.remove(&session);
        }
    }
}

/// The cookie `WebView::set_cookie` takes for `cookie`, always a domain
/// cookie: `wry` hands libsoup `cookie.domain()`, which strips exactly one
/// leading dot — hence the double dot here, so libsoup receives
/// `.example.com` and sends it to every subdomain too.
fn to_wry_cookie(cookie: &WebviewCookie) -> cookie::Cookie<'static> {
    let mut built = cookie::Cookie::build((cookie.name.clone(), cookie.value.clone()))
        .domain(format!("..{}", cookie.domain.trim_start_matches('.')))
        .path(cookie.path.clone())
        .secure(cookie.secure)
        .http_only(cookie.http_only);
    if let Some(same_site) = cookie.same_site {
        built = built.same_site(match same_site {
            WebviewSameSite::Strict => cookie::SameSite::Strict,
            WebviewSameSite::Lax => cookie::SameSite::Lax,
            WebviewSameSite::None => cookie::SameSite::None,
        });
    }
    if let Some(expires) = cookie
        .expires
        .and_then(|secs| cookie::time::OffsetDateTime::from_unix_timestamp(secs).ok())
    {
        built = built.expires(expires);
    }
    built.build()
}

/// One of the webview's cookies, or `None` without a domain.
/// `cookie::Cookie::domain()` strips the leading dot libsoup marks a
/// domain cookie with, so whether it was one is lost here.
fn from_wry_cookie(cookie: &cookie::Cookie<'_>) -> Option<WebviewCookie> {
    Some(WebviewCookie {
        name: cookie.name().to_string(),
        value: cookie.value().to_string(),
        domain: cookie.domain()?.trim_start_matches('.').to_string(),
        path: cookie.path().unwrap_or("/").to_string(),
        secure: cookie.secure() == Some(true),
        http_only: cookie.http_only() == Some(true),
        same_site: cookie.same_site().map(|same_site| match same_site {
            cookie::SameSite::Strict => WebviewSameSite::Strict,
            cookie::SameSite::Lax => WebviewSameSite::Lax,
            cookie::SameSite::None => WebviewSameSite::None,
        }),
        expires: cookie
            .expires_datetime()
            .map(|expires| expires.unix_timestamp()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn webview_cookie() -> WebviewCookie {
        WebviewCookie {
            name: "session".to_string(),
            value: "abc".to_string(),
            domain: "example.com".to_string(),
            path: "/app".to_string(),
            secure: true,
            http_only: true,
            same_site: Some(WebviewSameSite::Lax),
            expires: Some(1_900_000_000),
        }
    }

    #[test]
    fn to_wry_cookie_doubles_the_leading_dot_libsoup_strips_one_of() {
        let cookie = to_wry_cookie(&webview_cookie());

        assert_eq!(cookie.domain(), Some(".example.com"));
        assert_eq!(cookie.path(), Some("/app"));
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.http_only(), Some(true));
        assert_eq!(cookie.same_site(), Some(cookie::SameSite::Lax));
        assert_eq!(
            cookie.expires_datetime().map(|at| at.unix_timestamp()),
            Some(1_900_000_000)
        );
    }

    #[test]
    fn wry_cookie_round_trips() {
        let original = webview_cookie();

        assert_eq!(from_wry_cookie(&to_wry_cookie(&original)), Some(original));
    }

    #[test]
    fn from_wry_cookie_skips_a_cookie_without_domain() {
        assert!(from_wry_cookie(&cookie::Cookie::new("a", "b")).is_none());
    }

    /// A handler allowing every navigation and no popup, recording every
    /// message.
    struct Recording(Mutex<Vec<String>>);

    impl WebviewHandler for Recording {
        fn on_message(&self, body: String) {
            self.0.lock().unwrap().push(body);
        }
        fn allow_navigation(&self, _url: &str) -> bool {
            true
        }
        fn allow_new_window(&self, _url: &str) -> bool {
            false
        }
        fn on_download(&self, _url: &str) {}
        fn on_closed(&self) {}
    }

    fn session_cookies(value: &str) -> Vec<WebviewCookie> {
        let handler = Arc::new(Recording(Mutex::new(Vec::new())));
        let session = WryEngine
            .open_session(
                WebviewConfig {
                    visible: false,
                    cookies: vec![WebviewCookie {
                        value: value.to_string(),
                        path: "/".to_string(),
                        same_site: None,
                        expires: None,
                        ..webview_cookie()
                    }],
                },
                handler.clone(),
            )
            .unwrap();
        session.load_url("about:blank").unwrap();
        session
            .evaluate_script("window.ipc.postMessage('ready')")
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while handler.0.lock().unwrap().is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(*handler.0.lock().unwrap(), vec!["ready".to_string()]);
        let cookies = session.cookies().unwrap();
        session.close();
        cookies
    }

    /// Needs a display (X11/Wayland), so it only runs on demand:
    /// `cargo test -- --ignored concurrent_sessions`.
    #[test]
    #[ignore]
    fn concurrent_sessions_get_isolated_webviews_on_one_browser_thread() {
        let first = std::thread::spawn(|| session_cookies("first"));
        let second = std::thread::spawn(|| session_cookies("second"));
        let first = first.join().unwrap();
        let second = second.join().unwrap();

        assert_eq!(first.len(), 1, "{first:?}");
        assert_eq!(first[0].value, "first");
        assert_eq!(second.len(), 1, "{second:?}");
        assert_eq!(second[0].value, "second");

        // Both are closed by now; a later run reuses the same thread
        // (GTK can't be initialized again from a new one).
        let third = session_cookies("third");
        assert_eq!(third[0].value, "third");
    }
}
