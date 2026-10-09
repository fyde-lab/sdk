use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use uuid::Uuid;

use crate::Error;

/// What kind of script this is, opaque to the server beyond storage and
/// filtering (see `Script`/`CreateScriptRequest`/`UpdateScriptRequest`'s
/// `type` field in `../../../../api-protos/scripts/v1/scripts.proto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptType {
    /// Every script under `fyde-scripts`' `scrapers/` directory.
    Scraper,
    /// Every script under `fyde-scripts`' `parsers/` directory.
    Parser,
}

impl ScriptType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Scraper => "scraper",
            Self::Parser => "parser",
        }
    }
}

impl fmt::Display for ScriptType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ScriptType {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "scraper" => Ok(Self::Scraper),
            "parser" => Ok(Self::Parser),
            other => Err(Error::InvalidScriptType(other.to_string())),
        }
    }
}

/// What kind of value a [`ScriptParameter`] expects, so a client can render
/// the right form control (text field, number field, toggle) and coerce the
/// entered value before sending it back to the script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptParameterType {
    String,
    Number,
    Boolean,
}

/// A single configurable parameter a script declares it needs from the user
/// (e.g. a login username, an API key) before it can run. A script's full
/// set of these is carried on [`Script::parameters`], keyed by the
/// parameter's machine name (e.g. "username", "password") as the script
/// itself refers to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptParameter {
    /// Human-readable label for the form control, e.g. "Mot de passe".
    pub label: String,
    /// Example value shown in the empty form control, e.g. "••••••••".
    pub placeholder: String,
    pub parameter_type: ScriptParameterType,
    /// Whether the script requires this parameter to be set before it can
    /// run.
    pub required: bool,
    /// Whether the value is sensitive (e.g. a password) and should be
    /// masked/stored accordingly by the client. Opaque to the server beyond
    /// storage: it never sees the value either way.
    pub secret: bool,
}

/// A user-authored script, as returned by [`super::Service::fetch_script`]/
/// [`super::Service::list_user_scripts`]/[`super::Service::create_script`].
/// Unlike [`crate::Document`], script content is not encrypted client-side —
/// the server stores and can serve it in the clear, since scripts can be
/// marked public and shared between users.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Script {
    pub(super) id: Uuid,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) short_description: String,
    pub(super) is_public: bool,
    pub(super) icon: Vec<u8>,
    /// The only hosts (or subdomains of them) this script's `fyde.http`/
    /// `fyde.browser` calls may reach (see
    /// `../scrapers/host/mod.rs::is_host_allowed`). Empty means
    /// unrestricted.
    pub(super) allowed_domains: Vec<String>,
    pub(super) version: u64,
    pub(super) script: String,
    /// Unix timestamp, in seconds, of the last time this script was
    /// updated.
    pub(super) last_updated: i64,
    pub(super) script_type: ScriptType,
    /// The parameters this script needs from the user before it can run,
    /// keyed by the parameter's machine name.
    pub(super) parameters: HashMap<String, ScriptParameter>,
}

impl Script {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The script's own description, written by its author for display in
    /// a marketplace listing.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// A one-line summary, shorter than [`Self::description`], for display
    /// where space is limited (e.g. a marketplace list row).
    pub fn short_description(&self) -> &str {
        &self.short_description
    }

    pub fn is_public(&self) -> bool {
        self.is_public
    }

    pub fn icon(&self) -> &[u8] {
        &self.icon
    }

    pub fn allowed_domains(&self) -> &[String] {
        &self.allowed_domains
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn script(&self) -> &str {
        &self.script
    }

    pub fn last_updated(&self) -> i64 {
        self.last_updated
    }

    pub fn script_type(&self) -> ScriptType {
        self.script_type
    }

    pub fn parameters(&self) -> &HashMap<String, ScriptParameter> {
        &self.parameters
    }
}

/// Builds a [`Script`] filled with random-but-plausible data, for use in
/// tests.
#[cfg(test)]
pub(crate) struct FakeScript {
    script: Script,
}

#[cfg(test)]
impl FakeScript {
    pub(crate) fn new() -> Self {
        Self {
            script: Script {
                id: Uuid::now_v7(),
                name: crate::testing::random_word().to_string(),
                description: crate::testing::random_word().to_string(),
                short_description: crate::testing::random_word().to_string(),
                is_public: false,
                icon: crate::testing::random_bytes(16),
                allowed_domains: Vec::new(),
                version: 1,
                script: "return 1 + 1".to_string(),
                last_updated: crate::testing::random_past_timestamp(),
                script_type: ScriptType::Scraper,
                parameters: HashMap::new(),
            },
        }
    }

    pub(crate) fn with_script(mut self, script: &str) -> Self {
        self.script.script = script.to_string();
        self
    }

    pub(crate) fn with_type(mut self, script_type: ScriptType) -> Self {
        self.script.script_type = script_type;
        self
    }

    pub(crate) fn build(self) -> Script {
        self.script
    }
}
