//! End-user wording for every [`ErrorCode`], one translation per supported
//! [`Language`]. Kept short and non-technical on purpose: it's what the
//! application shows on screen, so it must never carry paths, ids, server
//! messages or anything else the technical error (`Error`'s `Display`)
//! does. Supported languages match the application's own translations
//! (`../application/shared/src/commonMain/composeResources/values*`).

use super::ErrorCode;

/// A language the SDK has error wording for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Language {
    English,
    French,
}

impl Language {
    /// Parses a short code (`"fr"`) or a locale (`"fr-FR"`, `"fr_FR.UTF-8"`),
    /// case-insensitively, falling back to [`Language::English`] for
    /// anything it doesn't recognize.
    pub(super) fn parse(language: &str) -> Self {
        let code = language
            .split(['-', '_', '.'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();

        match code.as_str() {
            "fr" => Language::French,
            _ => Language::English,
        }
    }
}

pub(super) fn user_message(code: ErrorCode, language: Language) -> &'static str {
    let (en, fr) = match code {
        ErrorCode::Io => (
            "A file on this device couldn't be read or written.",
            "Un fichier de cet appareil n'a pas pu être lu ou écrit.",
        ),
        ErrorCode::Database => (
            "The data stored on this device couldn't be accessed.",
            "Les données enregistrées sur cet appareil sont inaccessibles.",
        ),
        ErrorCode::DatabaseMigration => (
            "The data stored on this device couldn't be updated to this version of the app.",
            "Les données enregistrées sur cet appareil n'ont pas pu être mises à jour pour cette version de l'application.",
        ),
        ErrorCode::DataDirectory => (
            "The app's data folder couldn't be found on this device.",
            "Le dossier de données de l'application est introuvable sur cet appareil.",
        ),
        ErrorCode::Json => (
            "Some data is in an unexpected format.",
            "Certaines données sont dans un format inattendu.",
        ),
        ErrorCode::MessagePackEncode => (
            "Some data couldn't be prepared for saving.",
            "Certaines données n'ont pas pu être préparées pour l'enregistrement.",
        ),
        ErrorCode::MessagePackDecode => (
            "Some saved data couldn't be read back.",
            "Certaines données enregistrées n'ont pas pu être relues.",
        ),
        ErrorCode::InvalidUuid => ("An identifier is invalid.", "Un identifiant est invalide."),
        ErrorCode::InvalidInteger => ("A number is invalid.", "Un nombre est invalide."),
        ErrorCode::TaskFailed => (
            "A background task stopped unexpectedly.",
            "Une tâche en arrière-plan s'est arrêtée de manière inattendue.",
        ),
        ErrorCode::Encryption => (
            "Your data couldn't be encrypted or decrypted.",
            "Vos données n'ont pas pu être chiffrées ou déchiffrées.",
        ),
        ErrorCode::CredentialStore => (
            "This device's secure storage couldn't be accessed.",
            "Le stockage sécurisé de cet appareil est inaccessible.",
        ),
        ErrorCode::InvalidServerUrl => (
            "The server address is invalid.",
            "L'adresse du serveur est invalide.",
        ),
        ErrorCode::ServerUnreachable => (
            "Couldn't connect to the server. Check your internet connection.",
            "Impossible de se connecter au serveur. Vérifiez votre connexion internet.",
        ),
        ErrorCode::InvalidServerResponse => (
            "The server sent an unexpected response.",
            "Le serveur a envoyé une réponse inattendue.",
        ),
        ErrorCode::ServerUnavailable => (
            "The server is temporarily unavailable. Please try again later.",
            "Le serveur est temporairement indisponible. Veuillez réessayer plus tard.",
        ),
        ErrorCode::ServerTimeout => (
            "The server took too long to respond.",
            "Le serveur a mis trop de temps à répondre.",
        ),
        ErrorCode::RequestCancelled => ("The request was cancelled.", "La requête a été annulée."),
        ErrorCode::Unauthenticated => (
            "Your session has expired. Please log in again.",
            "Votre session a expiré. Veuillez vous reconnecter.",
        ),
        ErrorCode::PermissionDenied => (
            "You're not allowed to do this.",
            "Vous n'êtes pas autorisé à effectuer cette action.",
        ),
        ErrorCode::NotFoundOnServer => (
            "This item no longer exists on the server.",
            "Cet élément n'existe plus sur le serveur.",
        ),
        ErrorCode::AlreadyExists => (
            "This already exists. If it's a username, please choose another one.",
            "Cet élément existe déjà. S'il s'agit d'un nom d'utilisateur, veuillez en choisir un autre.",
        ),
        ErrorCode::InvalidRequest => (
            "The server rejected the request as invalid.",
            "Le serveur a rejeté la requête comme invalide.",
        ),
        ErrorCode::FailedPrecondition => (
            "This can't be done right now. Please try again.",
            "Cette action est impossible pour le moment. Veuillez réessayer.",
        ),
        ErrorCode::RateLimited => (
            "Too many attempts. Please wait a moment and try again.",
            "Trop de tentatives. Veuillez patienter un instant puis réessayer.",
        ),
        ErrorCode::NotSupportedByServer => (
            "The server doesn't support this feature yet.",
            "Le serveur ne prend pas encore en charge cette fonctionnalité.",
        ),
        ErrorCode::ServerError => (
            "Something went wrong on the server. Please try again later.",
            "Une erreur est survenue sur le serveur. Veuillez réessayer plus tard.",
        ),
        ErrorCode::InvalidCredentials => (
            "Wrong username or password.",
            "Nom d'utilisateur ou mot de passe incorrect.",
        ),
        ErrorCode::AlreadyLoggedIn => (
            "You're already logged in on this device. Log out first.",
            "Vous êtes déjà connecté sur cet appareil. Déconnectez-vous d'abord.",
        ),
        ErrorCode::UnsupportedDocumentExtension => (
            "This file type isn't supported. Only PDF files can be added.",
            "Ce type de fichier n'est pas pris en charge. Seuls les fichiers PDF peuvent être ajoutés.",
        ),
        ErrorCode::DocumentNotFound => (
            "This document couldn't be found.",
            "Ce document est introuvable.",
        ),
        ErrorCode::UnreadablePdf => (
            "This PDF file couldn't be read.",
            "Ce fichier PDF n'a pas pu être lu.",
        ),
        ErrorCode::InvalidChangelogEvent => (
            "A change received from the server couldn't be applied.",
            "Une modification reçue du serveur n'a pas pu être appliquée.",
        ),
        ErrorCode::InvalidSourceCategory => (
            "The document's category is not recognized.",
            "La catégorie du document n'est pas reconnue.",
        ),
        ErrorCode::InvalidSourceSubCategory => (
            "The document's sub-category is not recognized.",
            "La sous-catégorie du document n'est pas reconnue.",
        ),
        ErrorCode::InvalidPurpose => (
            "The document's purpose is not recognized.",
            "L'usage du document n'est pas reconnu.",
        ),
        ErrorCode::ScriptNotFound => (
            "This add-on couldn't be found.",
            "Cette extension est introuvable.",
        ),
        ErrorCode::MissingScriptParameter => (
            "This add-on is missing a required setting.",
            "Il manque un paramètre obligatoire à cette extension.",
        ),
        ErrorCode::InvalidScriptType => (
            "This add-on's type is not recognized.",
            "Le type de cette extension n'est pas reconnu.",
        ),
        ErrorCode::ScriptFailed => (
            "The add-on failed while running.",
            "L'extension a échoué pendant son exécution.",
        ),
        ErrorCode::BrowserFailed => (
            "The add-on's built-in browser ran into a problem.",
            "Le navigateur intégré de l'extension a rencontré un problème.",
        ),
    };

    match language {
        Language::English => en,
        Language::French => fr,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_accepts_short_codes_and_locales() {
        assert_eq!(Language::parse("fr"), Language::French);
        assert_eq!(Language::parse("FR"), Language::French);
        assert_eq!(Language::parse("fr-CA"), Language::French);
        assert_eq!(Language::parse("fr_FR.UTF-8"), Language::French);
        assert_eq!(Language::parse("en"), Language::English);
    }

    #[test]
    fn parse_falls_back_to_english() {
        assert_eq!(Language::parse("de"), Language::English);
        assert_eq!(Language::parse("C"), Language::English);
        assert_eq!(Language::parse(""), Language::English);
    }
}
