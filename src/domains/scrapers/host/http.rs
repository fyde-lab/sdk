use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mlua::{Lua, LuaSerdeExt, Result as LuaResult, Table, Value as LuaValue};
use serde_json::{Value, json};
use tokio::runtime::Handle;
use wreq::Client;
use wreq::cookie::Jar;
use wreq::header::{HeaderMap, HeaderName, HeaderValue};
use wreq_util::Profile;

use super::super::cookies::Cookie;
use super::super::models::RunOptions;
use super::super::reports::Recorder;

/// Builds a fresh cookie jar pre-populated from `cookies` (as previously
/// saved by `cookies::Service` for this scraper), so this run's `fyde.http`
/// client can pick up an already-authenticated session instead of being
/// forced to log in again.
pub(crate) fn build_jar(cookies: &[Cookie]) -> Arc<Jar> {
    let jar = Jar::default();
    for cookie in cookies {
        jar.add(cookie.set_cookie.as_str(), cookie.origin.as_str());
    }
    Arc::new(jar)
}

/// Exports every unexpired cookie `jar` holds for each origin in
/// `visited_origins` (see `track_origin` below), ready to be persisted via
/// `cookies::Service::save`. `Jar::get_all` can't be used here: by design it
/// drops the `Domain` of host-only cookies entirely (see its docs), which
/// would make them unreplayable against the right host on the next run.
/// Querying `matches()` per origin `fyde.http` actually visited instead means
/// a cookie's host is always known from the origin it was found under, even
/// when the jar itself doesn't carry it on the cookie. Ported from
/// `demo-rust-fyde`'s `host/session.rs::export_cookies`.
pub(crate) fn export_cookies(jar: &Jar, visited_origins: &Mutex<HashSet<String>>) -> Vec<Cookie> {
    let origins = match visited_origins.lock() {
        Ok(origins) => origins.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    };

    let mut cookies = Vec::new();
    for origin in origins {
        for cookie in jar.matches(origin.as_str()) {
            let mut set_cookie = format!("{}={}", cookie.name(), cookie.value());
            // Only set when the original cookie carried an explicit `Domain`
            // attribute (i.e. it isn't host-only) — reproduced here so
            // `Jar::add` restores the same scope on load instead of
            // narrowing a multi-subdomain cookie down to just this origin.
            if let Some(domain) = cookie.domain() {
                set_cookie.push_str(&format!("; Domain={domain}"));
            }
            if let Some(path) = cookie.path() {
                set_cookie.push_str(&format!("; Path={path}"));
            }
            if cookie.secure() {
                set_cookie.push_str("; Secure");
            }
            cookies.push(Cookie {
                origin: origin.clone(),
                set_cookie,
            });
        }
    }
    cookies
}

/// Records `url`'s origin (`scheme://host[:port]`) into the shared set
/// [`export_cookies`] later uses to know which hosts to ask the cookie jar
/// about when exporting cookies for the next run.
fn track_origin(origins: &Mutex<HashSet<String>>, url: &str) {
    if let Some(origin) = origin_of(url)
        && let Ok(mut origins) = origins.lock()
    {
        origins.insert(origin);
    }
}

fn origin_of(url: &str) -> Option<String> {
    let scheme_end = url.find("://")?;
    let rest = &url[scheme_end + 3..];
    let host_end = rest.find('/').unwrap_or(rest.len());
    Some(format!("{}://{}", &url[..scheme_end], &rest[..host_end]))
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

fn response_to_table(lua: &Lua, runtime: &Handle, resp: wreq::Response) -> LuaResult<Table> {
    let status = resp.status().as_u16();
    // The final URL after following any redirects - scripts need this to
    // know where they actually landed (e.g. to use as a Referer on a later
    // request, mimicking a real browser submitting a form from the page it
    // was just redirected to).
    let url = resp.uri().to_string();
    let body = runtime
        .block_on(resp.text())
        .map_err(mlua::Error::external)?;

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
            json!({ "action": action, "url": url, "message": message.clone() }),
        );
        return Err(mlua::Error::RuntimeError(message));
    }
    Ok(())
}

/// Records one `http_redirect` entry per hop in `resp`'s redirect chain —
/// `wreq` stashes it as a [`wreq::redirect::History`] response extension
/// whenever its (non-`none`) [`wreq::redirect::Policy`] actually follows a
/// redirect (see `table`'s client builder). Each entry is independent of the
/// final `http_request` entry `get`/`post_form`/`post_json`/`download` also
/// record, so a report shows every intermediate hop (e.g. an OAuth/OIDC
/// bounce) even though the script itself only ever sees the start URL and
/// the final one.
fn record_redirects(recorder: &Recorder, method: &str, resp: &wreq::Response) {
    let Some(history) = resp.extensions().get::<wreq::redirect::History>() else {
        return;
    };
    for hop in history {
        recorder.record(
            "http_redirect",
            json!({
                "method": method,
                "status": hop.status.as_u16(),
                "from": hop.previous.to_string(),
                "to": hop.uri.to_string(),
            }),
        );
    }
}

/// `fyde.http.get/post_form/post_json/download`, ported from
/// `demo-rust-fyde`'s own `host/http.rs` (same client configuration, cookie
/// jar, and Chrome TLS/HTTP2 fingerprint via `wreq`/`wreq_util`), plus
/// `post_json` (not present in `demo-rust-fyde`): some sites' login/session
/// APIs take a JSON body instead of a form-encoded one (confirmed against a
/// real login HAR capture for `caf/script.lua`), which `post_form`'s
/// hardcoded `.form(&form_map)` can't send — `post_json` takes any Lua
/// value (typically a table), converts it to `serde_json::Value` via
/// `LuaSerdeExt`, and sends it with `wreq`'s `.json(...)`, which also sets
/// `Content-Type: application/json` unless `headers` overrides it. `runtime`
/// is a handle onto the SDK's ambient tokio runtime — captured by
/// `service.rs` before the Lua VM moves onto its own blocking thread — used
/// to drive `wreq`'s async calls from these synchronous Lua callbacks,
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
/// `options.wreq_emulation` controls whether the client applies `wreq_util`'s
/// Chrome TLS/HTTP2 fingerprint emulation (`Profile::Chrome131`) — some
/// sites' WAFs block `wreq`'s TLS fingerprint outright regardless of
/// whether emulation is on, so a scraper can opt out per its
/// `scrapers/<name>/manifest.json`'s `wreq_emulation` field (in the
/// `scripts` repo) rather than carrying a global toggle.
///
/// `options.follow_redirects` controls whether the client follows HTTP redirects at
/// all. Unlike `reqwest`, `wreq`'s builder defaults to not following
/// redirects (`redirect::Policy::none()`); scripts generally rely on a
/// real-browser-like client that follows them (e.g. to pick up cookies set
/// along an OAuth/OIDC redirect chain before a login POST), so this matches
/// `reqwest`'s own default (`redirect::Policy::default()`) when `true`. A
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
/// against `allowed_domains` before following it (delegating to
/// `redirect::Policy::default()`'s own loop/max-hop handling once a hop
/// passes that check) — otherwise a page on an allowed domain could redirect
/// a request straight to a disallowed one and `wreq` would follow it
/// automatically, bypassing the check above entirely.
pub(super) fn table(
    lua: &Lua,
    jar: Arc<Jar>,
    visited_origins: Arc<Mutex<HashSet<String>>>,
    recorder: Arc<Recorder>,
    runtime: Handle,
    options: &RunOptions,
) -> LuaResult<Table> {
    let allowed_domains = Arc::new(options.allowed_domains.clone());
    let host = Arc::new(HttpHost {
        client: build_client(options, jar.clone(), allowed_domains.clone())?,
        jar,
        visited_origins,
        recorder,
        runtime,
        allowed_domains,
        debug_http_dump: options.debug_http_dump,
    });

    let http = lua.create_table()?;

    let get = host.clone();
    http.set(
        "get",
        lua.create_function(move |lua, (url, headers): (String, Option<Table>)| {
            get.admit("http.get", &url)?;
            let request = get.client.get(&url).headers(to_header_map(headers)?);
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
                let request = post_form
                    .client
                    .post(&url)
                    .headers(to_header_map(headers)?)
                    .form(&form_map);
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
                let request = post_json
                    .client
                    .post(&url)
                    .headers(to_header_map(headers)?)
                    .json(&body_json);
                post_json.send_text(lua, "http.post_json", "POST", &url, request, None)
            },
        )?,
    )?;

    // `set_cookie(url, set_cookie)` adds a cookie to this run's jar exactly as
    // if `url` had answered with a `Set-Cookie: <set_cookie>` header, for
    // sites that set a cookie from inline JS (e.g. a `document.cookie = ...`
    // anti-bot challenge) that a plain HTTP client never runs. A manual
    // `Cookie` request header can't stand in for this: `wreq` skips the jar
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
            let request = download.client.get(&url).headers(to_header_map(headers)?);
            let started = Instant::now();
            let resp = download.send("http.download", &url, request)?;

            let status = resp.status().as_u16();
            record_redirects(&download.recorder, "DOWNLOAD", &resp);
            let content_type = resp
                .headers()
                .get(wreq::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let bytes = download
                .runtime
                .block_on(resp.bytes())
                .map_err(|err| download.record_error("http.download", &url, err))?;

            // The body is never dumped here, even under `debug_http_dump`:
            // downloads are routinely binary (PDFs, images), which wouldn't
            // round-trip as JSON text anyway, unlike `get`/`post_form`'s
            // textual response bodies.
            download.recorder.record(
                "http_request",
                json!({
                    "method": "DOWNLOAD",
                    "url": url,
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

/// Builds this run's `wreq` client on `jar`, per `options` (see [`table`]'s
/// doc comment for `wreq_emulation`/`follow_redirects`), checking every
/// redirect hop against `allowed_domains`.
fn build_client(
    options: &RunOptions,
    jar: Arc<Jar>,
    allowed_domains: Arc<Vec<String>>,
) -> LuaResult<Client> {
    let mut builder = Client::builder().cookie_provider(jar);
    if options.wreq_emulation {
        builder = builder.emulation(Profile::Chrome131);
    }
    let redirect_policy = if options.follow_redirects {
        wreq::redirect::Policy::custom(move |attempt| {
            let target = attempt.uri.to_string();
            if let Err(message) = super::ensure_domain_allowed(&target, &allowed_domains) {
                return attempt.error(message);
            }
            // Delegates everything else (loop detection, the 10-hop cap) to
            // the default policy — `custom` doesn't get that for free.
            wreq::redirect::Policy::default().redirect(attempt)
        })
    } else {
        wreq::redirect::Policy::none()
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
    jar: Arc<Jar>,
    visited_origins: Arc<Mutex<HashSet<String>>>,
    recorder: Arc<Recorder>,
    runtime: Handle,
    allowed_domains: Arc<Vec<String>>,
    debug_http_dump: bool,
}

impl HttpHost {
    /// The gate every method passes before touching the network: rejects
    /// (and records) a `url` outside `allowed_domains`, then remembers its
    /// origin for [`export_cookies`].
    fn admit(&self, action: &str, url: &str) -> LuaResult<()> {
        ensure_domain_allowed_or_record(&self.recorder, action, url, &self.allowed_domains)?;
        track_origin(&self.visited_origins, url);
        Ok(())
    }

    /// Sends `request`, recording an `error` entry if it fails.
    fn send(
        &self,
        action: &str,
        url: &str,
        request: wreq::RequestBuilder,
    ) -> LuaResult<wreq::Response> {
        self.runtime
            .block_on(request.send())
            .map_err(|err| self.record_error(action, url, err))
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
        request: wreq::RequestBuilder,
        form_fields: Option<Vec<String>>,
    ) -> LuaResult<Table> {
        let started = Instant::now();
        let resp = self.send(action, url, request)?;
        let status = resp.status().as_u16();
        record_redirects(&self.recorder, method, &resp);
        let out = response_to_table(lua, &self.runtime, resp)?;

        let mut entry = json!({
            "method": method,
            "url": url,
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
    fn record_error(&self, action: &str, url: &str, err: wreq::Error) -> mlua::Error {
        self.recorder.record(
            "error",
            json!({ "action": action, "url": url, "message": err.to_string() }),
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
            Jar::default().into(),
            Arc::new(Mutex::new(HashSet::new())),
            recorder.clone(),
            runtime.handle().clone(),
            &RunOptions {
                debug_http_dump: false,
                wreq_emulation: true,
                follow_redirects: true,
                allowed_domains: vec!["allowed.example.com".to_string()],
                browser_visible: false,
            },
        )
        .unwrap();
        lua.globals().set("http", http_table).unwrap();

        let result: LuaResult<Table> = lua
            .load(r#"return http.get("https://evil.com/steal")"#)
            .eval();

        assert!(result.is_err());
        let report = recorder.finish();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].event, "error");
    }

    /// Spawns a tiny local server that unconditionally 302-redirects every
    /// connection to `location`, and returns the `http://127.0.0.1:<port>/`
    /// URL to hit it at. Used to prove the redirect policy itself checks
    /// `allowed_domains` on each hop, not just the request's starting URL —
    /// a real request/response round trip is the only way to exercise
    /// `wreq`'s own redirect-following machinery.
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
            Jar::default().into(),
            Arc::new(Mutex::new(HashSet::new())),
            recorder.clone(),
            runtime.handle().clone(),
            &RunOptions {
                debug_http_dump: false,
                wreq_emulation: false,
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
    fn get_follows_a_redirect_to_a_domain_inside_allowed_domains() {
        let runtime = runtime();
        let target_url = spawn_ok_server(&runtime, "hello");
        let redirect_url = spawn_redirecting_server(&runtime, &target_url);

        let lua = Lua::new();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let http_table = table(
            &lua,
            Jar::default().into(),
            Arc::new(Mutex::new(HashSet::new())),
            recorder.clone(),
            runtime.handle().clone(),
            &RunOptions {
                debug_http_dump: false,
                wreq_emulation: false,
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
        let jar: Arc<Jar> = Jar::default().into();
        let recorder = Arc::new(Recorder::new("didaxis"));
        let http_table = table(
            &lua,
            jar.clone(),
            Arc::new(Mutex::new(HashSet::new())),
            recorder,
            runtime.handle().clone(),
            &RunOptions {
                debug_http_dump: false,
                wreq_emulation: true,
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
        let matches: Vec<_> = jar.matches("https://allowed.example.com").collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].value(), "abc");

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

        let matches: Vec<_> = jar.matches("https://example.com").collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name(), "session");
        assert_eq!(matches[0].value(), "abc");
    }

    #[test]
    fn export_cookies_only_exports_visited_origins() {
        let cookies = vec![Cookie {
            origin: "https://example.com".to_string(),
            set_cookie: "session=abc; Path=/".to_string(),
        }];
        let jar = build_jar(&cookies);
        let visited_origins = Mutex::new(HashSet::new());

        assert_eq!(export_cookies(&jar, &visited_origins), Vec::new());

        visited_origins
            .lock()
            .unwrap()
            .insert("https://example.com".to_string());
        let exported = export_cookies(&jar, &visited_origins);

        assert_eq!(exported.len(), 1);
        assert_eq!(exported[0].origin, "https://example.com");
        assert!(exported[0].set_cookie.starts_with("session=abc"));
    }
}
