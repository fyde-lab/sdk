use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mlua::{Lua, Result as LuaResult, Table};
use tokio::runtime::Handle;
use wreq::Client;
use wreq::cookie::Jar;
use wreq::header::{HeaderMap, HeaderName, HeaderValue};
use wreq_util::Profile;

use super::super::cookies::Cookie;

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

/// `fyde.http.get/post_form/download`, ported from `demo-rust-fyde`'s own
/// `host/http.rs` (same client configuration, cookie jar, and Chrome
/// TLS/HTTP2 fingerprint via `wreq`/`wreq_util`), minus its debug report.
/// `runtime` is a handle onto the SDK's ambient tokio runtime — captured by
/// `service.rs` before the Lua VM moves onto its own blocking thread — used
/// to drive `wreq`'s async calls from these synchronous Lua callbacks,
/// instead of demo-rust-fyde's private, per-run `Runtime`.
pub(super) fn table(
    lua: &Lua,
    jar: Arc<Jar>,
    visited_origins: Arc<Mutex<HashSet<String>>>,
    runtime: Handle,
) -> LuaResult<Table> {
    let client = Client::builder()
        .cookie_provider(jar)
        .emulation(Profile::Chrome131)
        .timeout(Duration::from_secs(30))
        // Unlike `reqwest`, `wreq`'s builder defaults to not following
        // redirects at all (`redirect::Policy::none()`). Scripts rely on a
        // real-browser-like client that follows redirects (e.g. to pick up
        // cookies set along an OAuth/OIDC redirect chain before a login
        // POST), so match `reqwest`'s own default here.
        .redirect(wreq::redirect::Policy::default())
        .build()
        .expect("building the shared HTTP client");

    let http = lua.create_table()?;

    let get_client = client.clone();
    let get_runtime = runtime.clone();
    let get_origins = visited_origins.clone();
    http.set(
        "get",
        lua.create_function(move |lua, (url, headers): (String, Option<Table>)| {
            track_origin(&get_origins, &url);
            let header_map = to_header_map(headers)?;
            let resp = get_runtime
                .block_on(get_client.get(&url).headers(header_map).send())
                .map_err(mlua::Error::external)?;
            response_to_table(lua, &get_runtime, resp)
        })?,
    )?;

    let post_client = client.clone();
    let post_runtime = runtime.clone();
    let post_origins = visited_origins.clone();
    http.set(
        "post_form",
        lua.create_function(
            move |lua, (url, form, headers): (String, Table, Option<Table>)| {
                track_origin(&post_origins, &url);
                let form_map = table_to_string_map(&form)?;
                let header_map = to_header_map(headers)?;
                let resp = post_runtime
                    .block_on(
                        post_client
                            .post(&url)
                            .headers(header_map)
                            .form(&form_map)
                            .send(),
                    )
                    .map_err(mlua::Error::external)?;
                response_to_table(lua, &post_runtime, resp)
            },
        )?,
    )?;

    let download_client = client.clone();
    let download_runtime = runtime.clone();
    let download_origins = visited_origins.clone();
    http.set(
        "download",
        lua.create_function(move |lua, (url, headers): (String, Option<Table>)| {
            track_origin(&download_origins, &url);
            let header_map = to_header_map(headers)?;
            let resp = download_runtime
                .block_on(download_client.get(&url).headers(header_map).send())
                .map_err(mlua::Error::external)?;

            let status = resp.status().as_u16();
            let content_type = resp
                .headers()
                .get(wreq::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let bytes = download_runtime
                .block_on(resp.bytes())
                .map_err(mlua::Error::external)?;

            let out = lua.create_table()?;
            out.set("status", status)?;
            out.set("content_type", content_type)?;
            out.set("body", lua.create_string(&bytes)?)?;
            Ok(out)
        })?,
    )?;

    Ok(http)
}

#[cfg(test)]
mod tests {
    use super::*;

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
