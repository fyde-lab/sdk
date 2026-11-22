//! What `fyde.browser` needs from a real webview: its platform-specific
//! half.
//!
//! An engine only opens webviews, loads URLs, runs JS and reads/writes
//! cookies. Every decision (which navigations, popups and downloads to
//! allow, what JS to run, what a reply means, which cookies to keep) is the
//! driver's, made through the [`WebviewHandler`] it hands each session, so
//! the rules `fyde.browser` enforces live in one place whatever engine runs
//! them.
//!
//! An engine must:
//! - give every session its own, ephemeral data store (cookies, storage,
//!   cache), never shared with another session nor left on disk once it
//!   closes, and refuse to open one ([`WebviewEngine::open_session`] fails)
//!   where it can't;
//! - load the session's [`WebviewConfig::cookies`] before it returns from
//!   `open_session`, so no navigation can race them;
//! - expose `window.ipc.postMessage(string)` to every page it loads, handing
//!   the string to [`WebviewHandler::on_message`];
//! - ask [`WebviewHandler::allow_navigation`] before every top-level and
//!   subframe navigation (link, form submit, POST included, script,
//!   redirect), and block it on `false`;
//! - ask [`WebviewHandler::allow_new_window`] before opening any popup
//!   (`window.open`, `target="_blank"`), and never open it on `false`. A
//!   popup it opens is held to every rule here, answered by the session's
//!   own handler (its navigations, popups and downloads included), shares
//!   the session's data store, is shown only if [`WebviewConfig::visible`],
//!   and closes with the session. An engine that can't do all of that
//!   refuses the popup, still asking first (the driver may take its URL);
//! - cancel every native download before anything is written to disk,
//!   reporting its URL to [`WebviewHandler::on_download`];
//! - call [`WebviewHandler::on_closed`] once a session's webview goes away
//!   on its own (its window closed by the user, its process killed).
//!
//! Handler methods may be called from any thread, are cheap and never
//! block, so an engine can call them from its UI thread.

use std::sync::Arc;

#[cfg(test)]
use mockall::automock;

use crate::Result;

/// Opens webviews for `fyde.browser`. One engine serves every run; each run
/// opens at most one session, lazily on its first `fyde.browser` call.
#[cfg_attr(test, automock)]
pub trait WebviewEngine: Send + Sync {
    /// Opens a new, isolated webview (see the [module docs](self) for what
    /// that takes), blocking until it's ready, with `handler` answering its
    /// requests until it's closed.
    fn open_session(
        &self,
        config: WebviewConfig,
        handler: Arc<dyn WebviewHandler>,
    ) -> Result<Box<dyn WebviewSession>>;
}

/// The engine of a caller that passed none: `fyde.browser` fails.
pub(crate) struct Unavailable;

impl WebviewEngine for Unavailable {
    fn open_session(
        &self,
        _config: WebviewConfig,
        _handler: Arc<dyn WebviewHandler>,
    ) -> Result<Box<dyn WebviewSession>> {
        Err(crate::ErrorKind::Browser(
            "no browser engine is available on this platform".to_string(),
        )
        .into())
    }
}

/// One run's webview, opened by [`WebviewEngine::open_session`].
#[cfg_attr(test, automock)]
pub trait WebviewSession: Send + Sync {
    /// Starts loading `url`, without waiting for it to load.
    fn load_url(&self, url: &str) -> Result<()>;

    /// Runs `script` in the current page, without waiting for it to finish:
    /// it reports back through `window.ipc.postMessage` itself.
    fn evaluate_script(&self, script: &str) -> Result<()>;

    /// Every cookie the webview holds, whatever its host.
    fn cookies(&self) -> Result<Vec<WebviewCookie>>;

    /// Closes the webview and drops its data store. Called once, when the
    /// run ends; [`WebviewHandler::on_closed`] needn't follow.
    fn close(&self);
}

/// The driver's half of a session, which its engine calls back into.
#[cfg_attr(test, automock)]
pub trait WebviewHandler: Send + Sync {
    /// A page posted `body` through `window.ipc.postMessage`.
    fn on_message(&self, body: String);

    /// Whether the webview may navigate (a frame or the page) to `url`.
    fn allow_navigation(&self, url: &str) -> bool;

    /// Whether the webview may open a popup on `url`.
    fn allow_new_window(&self, url: &str) -> bool;

    /// The page started a native download of `url`, which the engine has
    /// cancelled.
    fn on_download(&self, url: &str);

    /// The webview went away on its own: every later call to its session
    /// fails, or does nothing.
    fn on_closed(&self);
}

/// How to open a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebviewConfig {
    /// Whether the webview is shown to the user: off for a run nobody asked
    /// to watch, which still has to run as if it were on screen.
    pub visible: bool,
    /// Loaded into the session's data store before it opens.
    pub cookies: Vec<WebviewCookie>,
}

/// A cookie, as an engine sets or reads it.
///
/// `fyde.browser` always restores a cookie as a domain cookie on `domain`
/// (sent to its subdomains too), since engines can't report whether one
/// they hold is host-only.
#[derive(Clone, PartialEq, Eq)]
pub struct WebviewCookie {
    pub name: String,
    pub value: String,
    /// Without a leading dot.
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<WebviewSameSite>,
    /// Unix seconds; `None` for a session cookie (or when an engine can't
    /// tell). Only setting a cookie honours it: `fyde.browser` saves the
    /// cookies it reads back without an expiry.
    pub expires: Option<i64>,
}

/// Cookies carry sessions: never print their values.
impl std::fmt::Debug for WebviewCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebviewCookie")
            .field("name", &self.name)
            .field("value", &"<redacted>")
            .field("domain", &self.domain)
            .field("path", &self.path)
            .field("secure", &self.secure)
            .field("http_only", &self.http_only)
            .field("same_site", &self.same_site)
            .field("expires", &self.expires)
            .finish()
    }
}

/// A cookie's `SameSite` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebviewSameSite {
    Strict,
    Lax,
    None,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;

    #[test]
    fn unavailable_refuses_to_open_a_session() {
        let handler: Arc<dyn WebviewHandler> = Arc::new(MockWebviewHandler::new());

        let err = match Unavailable.open_session(
            WebviewConfig {
                visible: false,
                cookies: Vec::new(),
            },
            handler,
        ) {
            Ok(_) => panic!("no engine must open no session"),
            Err(err) => err,
        };

        assert_eq!(err.code(), ErrorCode::BrowserFailed);
    }
}
