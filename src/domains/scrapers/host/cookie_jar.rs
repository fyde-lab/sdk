use std::sync::{Mutex, MutexGuard};

use cookie_store::{CookieDomain, CookieStore, RawCookie};
use reqwest::header::HeaderValue;
use url::Url;

/// One run's `fyde.http` cookies, `reqwest`'s cookie provider. Its own
/// `reqwest::cookie::Jar` stores them the same way (a `cookie_store`) but
/// only answers with a ready-made `Cookie` header, while saving a run's
/// cookies (`http::export_cookies`) needs each one's `Domain`, `Path` and
/// `Secure`.
#[derive(Default)]
pub(crate) struct CookieJar {
    store: Mutex<CookieStore>,
}

/// A cookie [`CookieJar::matches`] returns, with the scope it was stored
/// under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MatchedCookie {
    pub(crate) name: String,
    pub(crate) value: String,
    /// Its `Domain` attribute, `None` for a host-only cookie.
    pub(crate) domain: Option<String>,
    /// Its `Path` attribute, or the default path it got from the URL that
    /// set it.
    pub(crate) path: String,
    pub(crate) secure: bool,
}

impl CookieJar {
    fn store(&self) -> MutexGuard<'_, CookieStore> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Stores `set_cookie` (a `Set-Cookie` header's value) as if `url` had
    /// answered with it. One that doesn't parse, or that `url` may not set
    /// (another site's `Domain`, `Secure` over plain HTTP...), is ignored,
    /// as a browser would.
    pub(crate) fn add(&self, set_cookie: &str, url: &str) {
        let (Ok(cookie), Ok(url)) = (RawCookie::parse(set_cookie), Url::parse(url)) else {
            return;
        };
        let _ = self.store().insert_raw(&cookie, &url);
    }

    /// The unexpired cookies a request to `url` would send, matched on host,
    /// path and scheme as a browser does.
    pub(crate) fn matches(&self, url: &str) -> Vec<MatchedCookie> {
        let Ok(url) = Url::parse(url) else {
            return Vec::new();
        };
        self.store()
            .matches(&url)
            .into_iter()
            .map(|cookie| MatchedCookie {
                name: cookie.name().to_string(),
                value: cookie.value().to_string(),
                domain: match &cookie.domain {
                    CookieDomain::Suffix(domain) => Some(domain.clone()),
                    _ => None,
                },
                path: String::from(cookie.path.as_ref()),
                secure: cookie.secure().unwrap_or(false),
            })
            .collect()
    }
}

impl reqwest::cookie::CookieStore for CookieJar {
    fn set_cookies(&self, cookie_headers: &mut dyn Iterator<Item = &HeaderValue>, url: &Url) {
        let cookies = cookie_headers
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| RawCookie::parse(value.to_string()).ok());
        self.store().store_response_cookies(cookies, url);
    }

    fn cookies(&self, url: &Url) -> Option<HeaderValue> {
        let header = self
            .store()
            .get_request_values(url)
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        if header.is_empty() {
            return None;
        }
        HeaderValue::from_str(&header).ok()
    }
}

#[cfg(test)]
mod tests {
    use reqwest::cookie::CookieStore as _;

    use super::*;

    #[test]
    fn add_then_matches_a_host_only_cookie() {
        let jar = CookieJar::default();

        jar.add("session=abc; Path=/; Secure", "https://example.com/login");

        assert_eq!(
            jar.matches("https://example.com/"),
            vec![MatchedCookie {
                name: "session".to_string(),
                value: "abc".to_string(),
                domain: None,
                path: "/".to_string(),
                secure: true,
            }]
        );
        assert!(jar.matches("https://other.example.com/").is_empty());
        assert!(jar.matches("http://example.com/").is_empty());
    }

    #[test]
    fn matches_keeps_a_cookies_domain() {
        let jar = CookieJar::default();

        jar.add("sso=1; Domain=example.com", "https://login.example.com/");

        let matched = jar.matches("https://www.example.com/");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].domain.as_deref(), Some("example.com"));
    }

    #[test]
    fn matches_gives_a_cookie_without_path_its_default_path() {
        let jar = CookieJar::default();

        jar.add("step=2", "https://example.com/realms/a/login");

        assert!(jar.matches("https://example.com/").is_empty());
        let matched = jar.matches("https://example.com/realms/a/next");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].path, "/realms/a");
    }

    #[test]
    fn add_ignores_a_cookie_for_another_site() {
        let jar = CookieJar::default();

        jar.add("x=1; Domain=evil.example.net", "https://example.com/");

        assert!(jar.matches("https://evil.example.net/").is_empty());
        assert!(jar.matches("https://example.com/").is_empty());
    }

    #[test]
    fn serves_reqwest_the_cookies_it_set() {
        let jar = CookieJar::default();
        let url = Url::parse("https://example.com/").unwrap();
        let headers = [
            HeaderValue::from_static("a=1; Path=/"),
            HeaderValue::from_static("b=2; Path=/"),
        ];

        jar.set_cookies(&mut headers.iter(), &url);

        let header = jar.cookies(&url).unwrap();
        let mut pairs: Vec<_> = header.to_str().unwrap().split("; ").collect();
        pairs.sort();
        assert_eq!(pairs, vec!["a=1", "b=2"]);
        assert!(
            jar.cookies(&Url::parse("https://other.example.org/").unwrap())
                .is_none()
        );
    }
}
