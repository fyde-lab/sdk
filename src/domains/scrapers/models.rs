use std::fmt;
use std::str::FromStr;

use crate::error::Language;
use crate::{Error, ErrorKind};

/// Progress callback payload — see
/// [`crate::ClientConfig::on_scraper_progress`] — for a running Lua scraper
/// script's `fyde.progress.step`/`fyde.progress.update` calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    /// `fyde.progress.step(step)` — announces a new high-level stage of the
    /// scrape, one of a fixed set of [`ProgressStep`]s so the application
    /// can word it in the user's language.
    Step { step: ProgressStep },
    /// `fyde.progress.update(current, total)` — fine-grained progress within
    /// the current step (e.g. 3 of 12 documents downloaded).
    Update { current: i64, total: i64 },
}

/// A high-level stage of a scrape, as passed to `fyde.progress.step` by its
/// [`Self::as_str`] name (`fyde.progress.step("logging_in")`). Scripts can't
/// pass free text: every step has its own wording in every supported
/// language (see [`Self::user_message`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgressStep {
    /// Submitting credentials, or otherwise opening a session on the site.
    LoggingIn,
    /// Checking whether a session saved by a previous run is still valid.
    CheckingSession,
    /// Requesting or waiting for a one-time code (SMS, email, ...).
    WaitingForVerificationCode,
    /// Waiting for a captcha or anti-robot check to be solved.
    WaitingForCaptcha,
    /// Finding which documents are available.
    ListingDocuments,
    /// Downloading and saving documents.
    DownloadingDocuments,
    /// The scrape is over.
    Done,
}

impl ProgressStep {
    pub(crate) const ALL: &'static [ProgressStep] = &[
        Self::LoggingIn,
        Self::CheckingSession,
        Self::WaitingForVerificationCode,
        Self::WaitingForCaptcha,
        Self::ListingDocuments,
        Self::DownloadingDocuments,
        Self::Done,
    ];

    /// The name scripts pass to `fyde.progress.step`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::LoggingIn => "logging_in",
            Self::CheckingSession => "checking_session",
            Self::WaitingForVerificationCode => "waiting_for_verification_code",
            Self::WaitingForCaptcha => "waiting_for_captcha",
            Self::ListingDocuments => "listing_documents",
            Self::DownloadingDocuments => "downloading_documents",
            Self::Done => "done",
        }
    }

    /// The step's end-user wording in `language` (a short code like `"fr"`
    /// or a locale like `"fr_FR.UTF-8"`), falling back to English for any
    /// language the SDK has no wording for.
    pub fn user_message(self, language: &str) -> &'static str {
        let (en, fr) = match self {
            Self::LoggingIn => ("Logging in", "Connexion en cours"),
            Self::CheckingSession => (
                "Checking the existing session",
                "Vérification de la session existante",
            ),
            Self::WaitingForVerificationCode => (
                "Waiting for the verification code",
                "En attente du code de vérification",
            ),
            Self::WaitingForCaptcha => (
                "Waiting for the anti-robot check",
                "En attente de la vérification anti-robot",
            ),
            Self::ListingDocuments => ("Looking for documents", "Recherche des documents"),
            Self::DownloadingDocuments => ("Downloading documents", "Téléchargement des documents"),
            Self::Done => ("Done", "Terminé"),
        };

        match Language::parse(language) {
            Language::English => en,
            Language::French => fr,
        }
    }
}

impl fmt::Display for ProgressStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProgressStep {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|value| value.as_str() == s)
            .ok_or_else(|| ErrorKind::InvalidProgressStep(s.to_string()).into())
    }
}

/// The per-run switches [`super::Service::run`] takes (see its doc comment
/// for what each one does), carried together from there down to the Lua
/// host tables and the browser driver instead of as five loose arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RunOptions {
    pub(super) debug_http_dump: bool,
    pub(super) wreq_emulation: bool,
    pub(super) follow_redirects: bool,
    pub(super) allowed_domains: Vec<String>,
    pub(super) browser_visible: bool,
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_step_round_trips_through_its_name() {
        for step in ProgressStep::ALL {
            assert_eq!(step.as_str().parse::<ProgressStep>().unwrap(), *step);
        }
    }

    #[test]
    fn an_unknown_step_name_is_rejected() {
        assert!("Logging in".parse::<ProgressStep>().is_err());
    }

    #[test]
    fn every_step_has_a_distinct_wording_in_every_language() {
        for language in ["en", "fr"] {
            let mut seen = HashSet::new();
            for step in ProgressStep::ALL {
                let message = step.user_message(language);
                assert!(!message.is_empty(), "{step:?} has no {language} wording");
                assert!(
                    seen.insert(message),
                    "{step:?} shares its {language} wording with another step"
                );
            }
        }
    }

    #[test]
    fn user_message_is_localized_and_falls_back_to_english() {
        let step = ProgressStep::LoggingIn;

        assert_ne!(step.user_message("en"), step.user_message("fr"));
        assert_eq!(step.user_message("fr"), step.user_message("fr_FR.UTF-8"));
        assert_eq!(step.user_message("xx"), step.user_message("en"));
    }
}
