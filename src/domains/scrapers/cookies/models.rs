use serde::{Deserialize, Serialize};

/// One cookie captured from a scraper run, recorded against the origin
/// (`scheme://host`) it was seen on — enough to replay into a fresh cookie
/// jar on a later run (via `wreq::cookie::Jar::add`), reproducing the same
/// host-only-or-`Domain` scope it originally had. See
/// `host::http::{build_jar, export_cookies}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Cookie {
    pub(crate) origin: String,
    pub(crate) set_cookie: String,
}

/// Builds a [`Cookie`] filled with random-but-plausible data, overridable
/// field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeCookie {
    cookie: Cookie,
}

#[cfg(test)]
impl FakeCookie {
    pub(crate) fn new() -> Self {
        Self {
            cookie: Cookie {
                origin: format!("https://{}.example.com", crate::testing::random_word()),
                set_cookie: format!("session={}", crate::testing::random_hex(16)),
            },
        }
    }

    pub(crate) fn with_origin(mut self, origin: impl Into<String>) -> Self {
        self.cookie.origin = origin.into();
        self
    }

    pub(crate) fn with_set_cookie(mut self, set_cookie: impl Into<String>) -> Self {
        self.cookie.set_cookie = set_cookie.into();
        self
    }

    pub(crate) fn build(self) -> Cookie {
        self.cookie
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_cookie_builds_with_plausible_defaults() {
        let cookie = FakeCookie::new().build();

        assert!(cookie.origin.starts_with("https://"));
        assert!(cookie.set_cookie.starts_with("session="));
    }

    #[test]
    fn fake_cookie_with_methods_override_defaults() {
        let cookie = FakeCookie::new()
            .with_origin("https://example.com")
            .with_set_cookie("PHPSESSID=abc; Path=/")
            .build();

        assert_eq!(cookie.origin, "https://example.com");
        assert_eq!(cookie.set_cookie, "PHPSESSID=abc; Path=/");
    }
}
