mod browser;
mod documents;
mod html;
mod http;
mod input;
mod json;
mod log;
mod progress;

pub(super) use http::{build_jar, export_cookies};

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use mlua::{Lua, LuaSerdeExt, Table, Value as LuaValue};
use serde_json::Value as JsonValue;
use tokio::runtime::Handle;
use wreq::cookie::Jar;

use crate::domains::documents::Service as DocumentsService;
use crate::{ErrorContext as _, Result};

use super::ProgressEvent;
use super::browser::Service as BrowserService;
use super::cookies::Cookie;
use super::reports::Recorder;

/// Handle returned by [`install`], letting `service.rs` read back
/// `fyde.session`'s final contents and every cookie this run's `fyde.http`
/// picked up, once the script's `run` function has returned.
pub(super) struct Installed {
    cookie_jar: Arc<Jar>,
    visited_origins: Arc<Mutex<HashSet<String>>>,
}

/// Enforced by every `fyde.http.get/post_form/post_json/download` call and
/// `fyde.browser:open` — the one network entry point each of those has —
/// before it ever reaches the network: `url`'s host must equal, or be a
/// subdomain of, one of `allowed_domains` (conventionally a scraper's own
/// `scrapers/<name>/settings.json` `allowed_domains` list), or this returns
/// `Err` with a message describing the violation. Fails closed: an empty
/// `allowed_domains` allows nothing, so a scraper (or a server-supplied
/// script record) that forgets to declare its hosts can't reach arbitrary
/// hosts — including `localhost` or LAN addresses — by omission. See
/// [`is_host_allowed`] for how IP-literal hosts are matched. A script that
/// ignores the `Err` (doesn't wrap the call in `pcall`) has its `run`
/// function's call itself fail, per Lua's normal error propagation — exactly
/// like any other `fyde.*` error — which is what actually stops the script.
fn ensure_domain_allowed(url: &str, allowed_domains: &[String]) -> std::result::Result<(), String> {
    // `Url::host_str` already excludes userinfo/port and, per the WHATWG URL
    // Standard this crate implements, lowercases a domain host during
    // parsing — the explicit `to_ascii_lowercase()` in `is_host_allowed` is
    // only needed for `allowed_domains`' own entries.
    let host = url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string));
    let Some(host) = host else {
        return Err(format!("could not determine the host of url {url:?}"));
    };

    if is_host_allowed(&host, allowed_domains) {
        Ok(())
    } else {
        Err(format!(
            "host {host:?} (from url {url:?}) is not in this scraper's allowed_domains list {allowed_domains:?}"
        ))
    }
}

/// The host-only half of [`ensure_domain_allowed`] — `host` must equal, or
/// be a subdomain of, one of `allowed_domains`; an empty `allowed_domains`
/// allows nothing. An IP-literal `host` (e.g. `127.0.0.1`, `[::1]`) only ever
/// matches an identical entry, never by suffix — otherwise an entry like
/// `0.1` would let `10.0.0.1` through as a "subdomain" — so loopback/private
/// addresses are only reachable when a scraper lists that exact address.
/// Exposed beyond `host` (`pub(super)`, i.e. visible
/// throughout `scrapers`) for `browser::driver`'s `with_navigation_handler`/
/// `with_new_window_req_handler` callbacks, which only have a URL to parse
/// themselves — there's no shared request-building code path to hang
/// `ensure_domain_allowed`'s `Result`-returning, error-message-formatting
/// shape off of there.
pub(super) fn is_host_allowed(host: &str, allowed_domains: &[String]) -> bool {
    let is_ip_literal = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok();

    allowed_domains.iter().any(|domain| {
        let domain = domain.to_ascii_lowercase();
        host == domain || (!is_ip_literal && host.ends_with(&format!(".{domain}")))
    })
}

impl Installed {
    /// Reads `fyde.session` back out of the Lua VM as JSON, and every cookie
    /// now sitting in the jar for a host `fyde.http` actually visited this
    /// run — ready for `service.rs` to persist via `session::Service::save`/
    /// `cookies::Service::save`. Called once after the script's `run`
    /// function returns, whether it succeeded or failed, so state from a
    /// partial run isn't lost.
    pub(super) fn drain(&self, lua: &Lua) -> Result<(JsonValue, Vec<Cookie>)> {
        let fyde: Table = lua
            .globals()
            .get("fyde")
            .context("fetching the `fyde` global")?;
        let session_table: Table = fyde.get("session").context("fetching `fyde.session`")?;
        let session_data: JsonValue = lua
            .from_value(LuaValue::Table(session_table))
            .context("converting fyde.session to JSON")?;

        let cookies = export_cookies(&self.cookie_jar, &self.visited_origins);

        Ok((session_data, cookies))
    }
}

/// Wires every host capability a scraper script is allowed to use into a
/// single `fyde` global table: `fyde.log`, `fyde.http`, `fyde.html`,
/// `fyde.json`, `fyde.progress`, `fyde.input`, `fyde.save_document`,
/// `fyde.session`, `fyde.browser`. Scripts never reach outside this table —
/// no raw `io`/`os`/socket access from script code, enforced by the VM
/// itself: `service.rs`'s `sandboxed_lua` builds the `Lua` instance this is
/// installed into with those standard libraries never loaded in the first
/// place, not just unreferenced by convention — see
/// `demo-rust-fyde`'s own `host/mod.rs`, which this is ported from
/// (`fyde.browser` is the one exception with no `demo-rust-fyde`
/// counterpart — see `super::browser`). Unlike that port, `fyde.log`,
/// `fyde.http`, `fyde.progress`, `fyde.input` and `fyde.browser` each also
/// write an entry to `recorder` — the `reports` sub-domain's per-run debug
/// report, `demo-rust-fyde`'s `Report` brought back (`service.rs` saves it
/// once the script's `run` function returns). `debug_http_dump` is
/// forwarded to `fyde.http` and `fyde.browser` (see `host::http::table` and
/// `host::browser::table`) — it opts this run into also recording response
/// bodies for `fyde.http`, and the opened page's HTML for
/// `fyde.browser:open`, on top of the method/url/status/timing always
/// recorded. `wreq_emulation` is also forwarded to `fyde.http` only — it
/// toggles `wreq`'s Chrome TLS/HTTP2 fingerprint emulation. `follow_redirects` is also forwarded to `fyde.http` only — it
/// toggles whether the client automatically follows HTTP redirects.
/// `allowed_domains` is forwarded to both `fyde.http` and `fyde.browser` —
/// the only two tables with a network entry point of their own — and
/// enforced by `ensure_domain_allowed` before any of their methods actually
/// reaches the network: a scraper (conventionally reading its own
/// `scrapers/<name>/settings.json`'s `allowed_domains` list) can only ever
/// make requests to hosts it explicitly declared.
#[allow(clippy::too_many_arguments)]
pub(super) fn install(
    lua: &Lua,
    scraper_name: &str,
    session_data: JsonValue,
    cookies: Vec<Cookie>,
    documents: Arc<dyn DocumentsService>,
    browser: Arc<dyn BrowserService>,
    on_progress: Option<Arc<dyn Fn(ProgressEvent) + Send + Sync>>,
    on_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
    recorder: Arc<Recorder>,
    runtime: Handle,
    debug_http_dump: bool,
    wreq_emulation: bool,
    follow_redirects: bool,
    allowed_domains: Vec<String>,
) -> Result<Installed> {
    let cookie_jar = build_jar(&cookies);
    let visited_origins = Arc::new(Mutex::new(HashSet::new()));
    let allowed_domains = Arc::new(allowed_domains);

    let fyde = lua.create_table().context("creating the `fyde` table")?;

    fyde.set("log", log::table(lua, scraper_name, recorder.clone())?)
        .context("installing fyde.log")?;
    fyde.set(
        "http",
        http::table(
            lua,
            cookie_jar.clone(),
            visited_origins.clone(),
            recorder.clone(),
            runtime.clone(),
            debug_http_dump,
            wreq_emulation,
            follow_redirects,
            allowed_domains.clone(),
        )?,
    )
    .context("installing fyde.http")?;
    fyde.set("html", html::table(lua)?)
        .context("installing fyde.html")?;
    fyde.set("json", json::table(lua)?)
        .context("installing fyde.json")?;
    fyde.set(
        "progress",
        progress::table(lua, on_progress, recorder.clone())?,
    )
    .context("installing fyde.progress")?;
    fyde.set("input", input::table(lua, on_question, recorder.clone())?)
        .context("installing fyde.input")?;
    fyde.set(
        "save_document",
        documents::save_document_fn(lua, documents, scraper_name, runtime.clone())?,
    )
    .context("installing fyde.save_document")?;
    fyde.set(
        "browser",
        browser::table(
            lua,
            browser,
            recorder,
            runtime,
            debug_http_dump,
            allowed_domains,
        )?,
    )
    .context("installing fyde.browser")?;

    let session_value = lua
        .to_value(&session_data)
        .context("converting session data to a Lua value")?;
    let session_table = match session_value {
        LuaValue::Table(table) => table,
        _ => lua.create_table().context("creating empty session table")?,
    };
    fyde.set("session", session_table)
        .context("installing fyde.session")?;

    lua.globals()
        .set("fyde", fyde)
        .context("exposing the `fyde` global")?;

    Ok(Installed {
        cookie_jar,
        visited_origins,
    })
}

#[cfg(test)]
mod domain_guard_tests {
    use super::*;

    #[test]
    fn ensure_domain_allowed_ignores_the_urls_port_and_userinfo() {
        let allowed = vec!["example.com".to_string()];
        assert!(ensure_domain_allowed("https://Example.com:8443/a/b?c=1", &allowed).is_ok());
        assert!(ensure_domain_allowed("https://user:pass@example.com/login", &allowed).is_ok());
    }

    #[test]
    fn ensure_domain_allowed_rejects_an_unparseable_url() {
        let allowed = vec!["example.com".to_string()];
        assert!(ensure_domain_allowed("not a url", &allowed).is_err());
    }

    #[test]
    fn ensure_domain_allowed_allows_nothing_when_the_list_is_empty() {
        assert!(ensure_domain_allowed("https://anything.example/at/all", &[]).is_err());
        assert!(ensure_domain_allowed("http://127.0.0.1:8080/", &[]).is_err());
    }

    #[test]
    fn ensure_domain_allowed_only_matches_an_ip_literal_exactly() {
        let allowed = vec!["0.1".to_string(), "127.0.0.1".to_string()];
        assert!(ensure_domain_allowed("http://10.0.0.1/", &allowed).is_err());
        assert!(ensure_domain_allowed("http://127.0.0.1:8080/", &allowed).is_ok());
    }

    #[test]
    fn ensure_domain_allowed_rejects_an_unlisted_ipv6_literal() {
        let allowed = vec!["example.com".to_string()];
        assert!(ensure_domain_allowed("http://[::1]/", &allowed).is_err());
        assert!(ensure_domain_allowed("http://[::1]/", &["[::1]".to_string()]).is_ok());
    }

    #[test]
    fn ensure_domain_allowed_accepts_an_exact_match() {
        let allowed = vec!["example.com".to_string()];
        assert!(ensure_domain_allowed("https://example.com/login", &allowed).is_ok());
    }

    #[test]
    fn ensure_domain_allowed_accepts_a_subdomain_of_an_allowed_domain() {
        let allowed = vec!["impots.gouv.fr".to_string()];
        assert!(ensure_domain_allowed("https://cfspart-idp.impots.gouv.fr/", &allowed).is_ok());
    }

    #[test]
    fn ensure_domain_allowed_rejects_an_unrelated_host() {
        let allowed = vec!["example.com".to_string()];
        let err = ensure_domain_allowed("https://evil.com/steal", &allowed).unwrap_err();
        assert!(err.contains("evil.com"));
    }

    #[test]
    fn ensure_domain_allowed_rejects_a_superdomain_of_an_allowed_domain() {
        // Allowing "sub.example.com" must not also allow "example.com" or
        // some unrelated host that merely ends with the same suffix.
        let allowed = vec!["sub.example.com".to_string()];
        assert!(ensure_domain_allowed("https://example.com/", &allowed).is_err());
        assert!(ensure_domain_allowed("https://notsub.example.com/", &allowed).is_err());
    }
}
