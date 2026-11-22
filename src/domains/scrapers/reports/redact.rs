//! Keeps secrets a URL may carry out of debug reports and logs.

/// `url` without what may carry a secret: its query string (OAuth codes,
/// access tokens, signed download links...), fragment (implicit-flow
/// tokens) and userinfo. A dropped query or fragment is marked, so a report
/// still shows there was one. A `url` that doesn't parse keeps only what
/// comes before its first `?` or `#`, without anything up to an `@` in its
/// authority (see [`without_userinfo`]). A URL with no host to keep (`data:`,
/// `blob:`, ...) keeps only its scheme: a `data:` URL is the content itself.
pub(crate) fn redact_url(url: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        let (kept, redacted) = match url.find(['?', '#']) {
            Some(end) => (&url[..end], REDACTED),
            None => (url, ""),
        };
        return format!("{}{redacted}", without_userinfo(kept));
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

/// `url` (one that doesn't parse, already cut at its query) without the
/// userinfo its authority may start with: everything up to its last `@`
/// before the path, the authority starting after a `//` (or at the start,
/// without one), e.g. `//user:password@host/path` gives `//host/path`.
fn without_userinfo(url: &str) -> String {
    let start = url.find("//").map_or(0, |at| at + 2);
    let end = url[start..].find('/').map_or(url.len(), |at| start + at);
    match url[start..end].rfind('@') {
        Some(at) => format!("{}{}", &url[..start], &url[start + at + 1..]),
        None => url.to_string(),
    }
}

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

    #[test]
    fn redact_url_drops_the_userinfo_of_an_unparseable_url() {
        assert_eq!(
            redact_url("//user:hunter2@example.com/a?token=secret"),
            "//example.com/a?[redacted]"
        );
        assert_eq!(
            redact_url("https://user:hunter2@exa mple.com/a"),
            "https://exa mple.com/a"
        );
        assert_eq!(redact_url("user hunter2@example.com/a"), "example.com/a");
        assert_eq!(redact_url("/relative/a@b"), "/relative/a@b");
    }
}
