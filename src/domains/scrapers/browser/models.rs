/// The result of [`super::Service::submit`] — the HTTP response a scraper
/// script's in-page `fetch()` form submission received back, read out of
/// the webview's own JS engine via `window.ipc.postMessage` (see
/// `super::driver`). Mirrors `fyde.http`'s own response table shape
/// (`host::http::response_to_table`) so a script can treat the two the same
/// way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrowserResponse {
    pub(crate) status: u16,
    pub(crate) url: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: String,
}

/// Builds a [`BrowserResponse`] filled with random-but-plausible data,
/// overridable field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeBrowserResponse {
    response: BrowserResponse,
}

#[cfg(test)]
impl FakeBrowserResponse {
    pub(crate) fn new() -> Self {
        Self {
            response: BrowserResponse {
                status: 200,
                url: format!("https://{}.example.com/", crate::testing::random_word()),
                headers: Vec::new(),
                body: crate::testing::random_word().to_string(),
            },
        }
    }

    pub(crate) fn with_status(mut self, status: u16) -> Self {
        self.response.status = status;
        self
    }

    pub(crate) fn with_body(mut self, body: impl Into<String>) -> Self {
        self.response.body = body.into();
        self
    }

    pub(crate) fn build(self) -> BrowserResponse {
        self.response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_browser_response_builds_with_plausible_defaults() {
        let response = FakeBrowserResponse::new().build();

        assert_eq!(response.status, 200);
        assert!(response.url.starts_with("https://"));
    }

    #[test]
    fn fake_browser_response_with_methods_override_defaults() {
        let response = FakeBrowserResponse::new()
            .with_status(500)
            .with_body("boom")
            .build();

        assert_eq!(response.status, 500);
        assert_eq!(response.body, "boom");
    }
}
