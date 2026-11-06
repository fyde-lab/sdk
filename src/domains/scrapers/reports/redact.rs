//! Keeps secrets a URL may carry out of debug reports and logs.

/// `url` without what may carry a secret: its query string (OAuth codes,
/// access tokens, signed download links...), fragment (implicit-flow
/// tokens) and userinfo. A dropped query or fragment is marked, so a report
/// still shows there was one. A `url` that doesn't parse keeps only what
/// comes before its first `?` or `#`. A URL with no host to keep (`data:`,
/// `blob:`, ...) keeps only its scheme: a `data:` URL is the content itself.
pub(crate) fn redact_url(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return match url.find(['?', '#']) {
            Some(end) => format!("{}{REDACTED}", &url[..end]),
            None => url.to_string(),
        };
    };
    if parsed.cannot_be_a_base() {
        return format!("{}:[redacted]", parsed.scheme());
    }
    let had_secret_parts = parsed.query().is_some() || parsed.fragment().is_some();
    parsed.set_query(None);
    parsed.set_fragment(None);
    // Only fails for URLs that can't have userinfo at all.
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    if had_secret_parts {
        format!("{parsed}{REDACTED}")
    } else {
        parsed.to_string()
    }
}

/// What stands in for a dropped query string or fragment.
const REDACTED: &str = "?[redacted]";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_url_drops_the_query_fragment_and_userinfo() {
        assert_eq!(
            redact_url("https://user:pass@example.com/cb?code=secret&state=x#token=t"),
            "https://example.com/cb?[redacted]"
        );
    }

    #[test]
    fn redact_url_keeps_a_url_without_secrets_as_is() {
        assert_eq!(
            redact_url("https://example.com:8443/a/b"),
            "https://example.com:8443/a/b"
        );
    }

    #[test]
    fn redact_url_keeps_only_the_scheme_of_a_url_without_a_host() {
        assert_eq!(
            redact_url("data:application/pdf;base64,JVBERi0x"),
            "data:[redacted]"
        );
        assert_eq!(
            redact_url("blob:https://example.com/550e8400"),
            "blob:[redacted]"
        );
    }

    #[test]
    fn redact_url_cuts_an_unparseable_url_at_its_query() {
        assert_eq!(redact_url("/relative?token=secret"), "/relative?[redacted]");
        assert_eq!(redact_url("not a url"), "not a url");
    }
}
