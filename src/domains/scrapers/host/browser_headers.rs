//! The headers `fyde.http` sends under `RunOptions::browser_emulation`: a
//! desktop Firefox's, in its order, so a request looks like one a browser
//! sent rather than one with no `User-Agent` at all. Firefox rather than
//! Chrome because it sends no client hints (`sec-ch-ua`, whose brand list
//! changes with every version) and no GREASE, so rustls' ClientHello and
//! these headers tell less of a different story than they would claiming to
//! be Chrome. The TLS and HTTP/2 fingerprints stay rustls' and hyper's own.
//!
//! Headers a script passes always win over these. `reqwest` adds
//! `Accept-Encoding` (the same codings Firefox accepts), `Content-Type` for a
//! body and `Cookie` from the jar.

use reqwest::header::{
    ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderName, HeaderValue, ORIGIN, REFERER,
    UPGRADE_INSECURE_REQUESTS, USER_AGENT,
};
use url::Url;

/// Firefox 160 (stable in November 2026) on Windows, the most common
/// desktop. Bump it now and then: a browser long out of date stands out too.
const FIREFOX_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:160.0) Gecko/20100101 Firefox/160.0";
/// French first: the sites scrapers visit are French, as are their users.
const FIREFOX_ACCEPT_LANGUAGE: &str = "fr,fr-FR;q=0.8,en-US;q=0.5,en;q=0.3";
const NAVIGATION_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
const FETCH_ACCEPT: &str = "application/json, text/plain, */*";

const SEC_FETCH_DEST: HeaderName = HeaderName::from_static("sec-fetch-dest");
const SEC_FETCH_MODE: HeaderName = HeaderName::from_static("sec-fetch-mode");
const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");
const SEC_FETCH_USER: HeaderName = HeaderName::from_static("sec-fetch-user");
const PRIORITY: HeaderName = HeaderName::from_static("priority");

/// What kind of browser request a `fyde.http` call stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RequestKind {
    /// Opening a page or following a link (`get`, `download`).
    Navigation,
    /// Submitting a form (`post_form`): a navigation that posts.
    FormSubmission,
    /// A page's script calling an API (`post_json`).
    Fetch,
}

/// The headers a Firefox would send for a `kind` request to `url`, with
/// `script`'s (the ones the script passed) replacing or adding to them.
/// `Sec-Fetch-Site`, and `Origin` on a POST, follow the `Referer` the script
/// passed, if any: without one, a navigation reads as typed in the address
/// bar and a fetch as coming from the site's own page.
pub(super) fn browser_headers(kind: RequestKind, url: &str, script: &HeaderMap) -> HeaderMap {
    let referer = script
        .get(REFERER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Url::parse(value).ok());
    let target = Url::parse(url).ok();

    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(FIREFOX_USER_AGENT));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static(match kind {
            RequestKind::Navigation | RequestKind::FormSubmission => NAVIGATION_ACCEPT,
            RequestKind::Fetch => FETCH_ACCEPT,
        }),
    );
    headers.insert(
        ACCEPT_LANGUAGE,
        HeaderValue::from_static(FIREFOX_ACCEPT_LANGUAGE),
    );
    if kind != RequestKind::Navigation
        && let Some(origin) = referer.as_ref().map(|r| r.origin().ascii_serialization())
        && let Ok(origin) = HeaderValue::from_str(&origin)
    {
        headers.insert(ORIGIN, origin);
    }
    let site = HeaderValue::from_static(fetch_site(kind, target.as_ref(), referer.as_ref()));
    match kind {
        RequestKind::Navigation | RequestKind::FormSubmission => {
            headers.insert(UPGRADE_INSECURE_REQUESTS, HeaderValue::from_static("1"));
            headers.insert(SEC_FETCH_DEST, HeaderValue::from_static("document"));
            headers.insert(SEC_FETCH_MODE, HeaderValue::from_static("navigate"));
            headers.insert(SEC_FETCH_SITE, site);
            headers.insert(SEC_FETCH_USER, HeaderValue::from_static("?1"));
            headers.insert(PRIORITY, HeaderValue::from_static("u=0, i"));
        }
        RequestKind::Fetch => {
            headers.insert(SEC_FETCH_DEST, HeaderValue::from_static("empty"));
            headers.insert(SEC_FETCH_MODE, HeaderValue::from_static("cors"));
            headers.insert(SEC_FETCH_SITE, site);
        }
    }

    for (name, value) in script {
        headers.insert(name.clone(), value.clone());
    }
    headers
}

/// `Sec-Fetch-Site` for a request to `target` from a page at `referer`.
fn fetch_site(kind: RequestKind, target: Option<&Url>, referer: Option<&Url>) -> &'static str {
    let (Some(target), Some(referer)) = (target, referer) else {
        return match kind {
            RequestKind::Navigation | RequestKind::FormSubmission => "none",
            RequestKind::Fetch => "same-origin",
        };
    };
    if target.origin() == referer.origin() {
        "same-origin"
    } else if target.scheme() == referer.scheme() && site_of(target) == site_of(referer) {
        "same-site"
    } else {
        "cross-site"
    }
}

/// `url`'s host's last two labels, standing in for its registrable domain
/// (`login.impots.gouv.fr` → `gouv.fr` is too wide, but only ever turns a
/// `cross-site` into a `same-site`, which a site can't hold against a
/// visitor). An IP address is its own site.
fn site_of(url: &Url) -> Option<String> {
    match url.host()? {
        url::Host::Domain(domain) => {
            let labels: Vec<&str> = domain.split('.').collect();
            Some(labels[labels.len().saturating_sub(2)..].join("."))
        }
        host => Some(host.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(headers: &[(&'static str, &'static str)]) -> HeaderMap {
        headers
            .iter()
            .map(|(name, value)| {
                (
                    HeaderName::from_static(name),
                    HeaderValue::from_static(value),
                )
            })
            .collect()
    }

    fn get<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
        headers.get(name).map(|value| value.to_str().unwrap())
    }

    #[test]
    fn a_navigation_without_referer_reads_as_typed_in() {
        let headers = browser_headers(
            RequestKind::Navigation,
            "https://example.com/",
            &HeaderMap::new(),
        );

        assert_eq!(get(&headers, "user-agent"), Some(FIREFOX_USER_AGENT));
        assert_eq!(get(&headers, "accept"), Some(NAVIGATION_ACCEPT));
        assert_eq!(get(&headers, "sec-fetch-mode"), Some("navigate"));
        assert_eq!(get(&headers, "sec-fetch-site"), Some("none"));
        assert_eq!(get(&headers, "origin"), None);
    }

    #[test]
    fn headers_come_in_firefoxs_order() {
        let headers = browser_headers(
            RequestKind::Navigation,
            "https://example.com/",
            &HeaderMap::new(),
        );

        let names: Vec<&str> = headers.keys().map(HeaderName::as_str).collect();
        assert_eq!(
            names,
            vec![
                "user-agent",
                "accept",
                "accept-language",
                "upgrade-insecure-requests",
                "sec-fetch-dest",
                "sec-fetch-mode",
                "sec-fetch-site",
                "sec-fetch-user",
                "priority",
            ]
        );
    }

    #[test]
    fn a_form_submission_gets_its_referers_origin() {
        let headers = browser_headers(
            RequestKind::FormSubmission,
            "https://example.com/login",
            &script(&[("referer", "https://example.com/login?next=/home")]),
        );

        assert_eq!(get(&headers, "origin"), Some("https://example.com"));
        assert_eq!(get(&headers, "sec-fetch-site"), Some("same-origin"));
        assert_eq!(
            get(&headers, "referer"),
            Some("https://example.com/login?next=/home")
        );
    }

    #[test]
    fn a_fetch_reads_as_from_the_sites_own_page() {
        let headers = browser_headers(
            RequestKind::Fetch,
            "https://api.example.com/session",
            &HeaderMap::new(),
        );

        assert_eq!(get(&headers, "accept"), Some(FETCH_ACCEPT));
        assert_eq!(get(&headers, "sec-fetch-mode"), Some("cors"));
        assert_eq!(get(&headers, "sec-fetch-site"), Some("same-origin"));
        assert_eq!(get(&headers, "upgrade-insecure-requests"), None);
    }

    #[test]
    fn fetch_site_compares_the_referers_site() {
        let target = Url::parse("https://api.example.com/").unwrap();
        let sibling = Url::parse("https://www.example.com/").unwrap();
        let other = Url::parse("https://example.org/").unwrap();
        let plain = Url::parse("http://www.example.com/").unwrap();

        let site = |referer: &Url| fetch_site(RequestKind::Fetch, Some(&target), Some(referer));

        assert_eq!(site(&target), "same-origin");
        assert_eq!(site(&sibling), "same-site");
        assert_eq!(site(&other), "cross-site");
        assert_eq!(site(&plain), "cross-site");
    }

    #[test]
    fn the_scripts_headers_win() {
        let headers = browser_headers(
            RequestKind::Fetch,
            "https://example.com/",
            &script(&[("accept", "*/*"), ("x-requested-with", "XMLHttpRequest")]),
        );

        assert_eq!(get(&headers, "accept"), Some("*/*"));
        assert_eq!(get(&headers, "x-requested-with"), Some("XMLHttpRequest"));
        assert_eq!(headers.get_all("accept").iter().count(), 1);
    }
}
