//! Where every `reqwest` client of the SDK starts (`fyde.http` in
//! `domains/scrapers/host/http.rs`, model downloads in `tools::models`).
//!
//! `reqwest` is built with `rustls-no-provider`, so its rustls TLS runs on
//! whichever crypto provider the process has installed: [`client_builder`]
//! installs `ring` (the provider tonic's gRPC channel already uses) unless
//! one is there already, rather than `reqwest`'s default `aws-lc-rs`, whose
//! C build is the slowest step of the SDK's. `reqwest` keeps its own TLS
//! settings and its platform certificate verifier.

/// A `reqwest` client builder, `ring` installed as rustls' provider first.
/// A provider the embedding application installed before stays: rustls
/// only takes the first, which is fine, as either can run the TLS.
pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_builder_installs_a_provider_reqwest_builds_on() {
        client_builder().build().unwrap();

        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
