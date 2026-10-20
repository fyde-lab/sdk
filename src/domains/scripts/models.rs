use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::{Error, ErrorKind};

/// What kind of script this is, opaque to the server beyond storage and
/// filtering (see `Script`/`CreateScriptRequest`/`UpdateScriptRequest`'s
/// `type` field in `../../../../server/protos/scripts/v1/scripts.proto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
            other => Err(ErrorKind::InvalidScriptType(other.to_string()).into()),
        }
    }
}

/// What kind of value a [`ScriptParameter`] expects, so a client can render
/// the right form control (text field, number field, toggle) and coerce the
/// entered value before sending it back to the script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Script {
    pub(super) id: Uuid,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) short_description: String,
    pub(super) is_public: bool,
    /// The id of the script's icon, fetched with
    /// [`crate::IconsService::fetch_icon`].
    pub(super) icon_id: Option<Uuid>,
    /// The only hosts (or subdomains of them) this script's `fyde.http`/
    /// `fyde.browser` calls may reach (see
    /// `../scrapers/host/mod.rs::is_host_allowed`). Empty blocks every
    /// host.
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

    /// The id of the script's icon, to fetch with
    /// [`crate::IconsService::fetch_icon`], `None` if it has none.
    pub fn icon_id(&self) -> Option<Uuid> {
        self.icon_id
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

/// Everything about a [`Script`] but its content ([`Script::script`]), as
/// returned by [`super::Service::list_user_scripts`]/
/// [`super::Service::list_public_scripts`]. Fetch the script itself with
/// [`super::Service::fetch_script`] to run it, and its icon with
/// [`crate::IconsService::fetch_icon`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptMetadata {
    pub(super) id: Uuid,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) short_description: String,
    pub(super) is_public: bool,
    /// See [`Script::icon_id`].
    pub(super) icon_id: Option<Uuid>,
    /// See [`Script::allowed_domains`].
    pub(super) allowed_domains: Vec<String>,
    pub(super) version: u64,
    /// Unix timestamp, in seconds, of the last time this script was
    /// updated.
    pub(super) last_updated: i64,
    pub(super) script_type: ScriptType,
    /// See [`Script::parameters`].
    pub(super) parameters: HashMap<String, ScriptParameter>,
}

impl ScriptMetadata {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// See [`Script::description`].
    pub fn description(&self) -> &str {
        &self.description
    }

    /// See [`Script::short_description`].
    pub fn short_description(&self) -> &str {
        &self.short_description
    }

    pub fn is_public(&self) -> bool {
        self.is_public
    }

    /// See [`Script::icon_id`].
    pub fn icon_id(&self) -> Option<Uuid> {
        self.icon_id
    }

    pub fn allowed_domains(&self) -> &[String] {
        &self.allowed_domains
    }

    pub fn version(&self) -> u64 {
        self.version
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

impl From<Script> for ScriptMetadata {
    fn from(script: Script) -> Self {
        Self {
            id: script.id,
            name: script.name,
            description: script.description,
            short_description: script.short_description,
            is_public: script.is_public,
            icon_id: script.icon_id,
            allowed_domains: script.allowed_domains,
            version: script.version,
            last_updated: script.last_updated,
            script_type: script.script_type,
            parameters: script.parameters,
        }
    }
}

/// What [`super::Service::install_script`] takes: the id of the script to
/// install, plus the values to run it with, keyed by the same machine names
/// as [`Script::parameters`] (e.g. `{"username": "alice"}`).
#[derive(Debug, Clone, PartialEq)]
pub struct InstallScriptRequest {
    pub script_id: Uuid,
    pub parameters: HashMap<String, Value>,
}

/// A script installed for the authenticated user, as persisted locally once
/// its `InstallScript` changelog event has been consumed (see
/// [`super::Service::list_installed_scripts`]): the full script, plus the
/// parameter values it was installed with, so it can be run without asking
/// for them again. Also the exact JSON payload carried, encrypted, by that
/// changelog event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstalledScript {
    pub(super) script: Script,
    pub(super) parameters: HashMap<String, Value>,
}

impl InstalledScript {
    pub fn script(&self) -> &Script {
        &self.script
    }

    /// The values to run [`Self::script`] with, keyed by the parameter's
    /// machine name.
    pub fn parameters(&self) -> &HashMap<String, Value> {
        &self.parameters
    }
}

/// Builds a [`Script`] filled with random-but-plausible data, for use in
/// tests.
#[cfg(any(test, feature = "testing"))]
pub struct FakeScript {
    script: Script,
}

#[cfg(any(test, feature = "testing"))]
impl Default for FakeScript {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "testing"))]
impl FakeScript {
    pub fn new() -> Self {
        Self {
            script: Script {
                id: Uuid::now_v7(),
                name: crate::tools::testing::random_word().to_string(),
                description: crate::tools::testing::random_word().to_string(),
                short_description: crate::tools::testing::random_word().to_string(),
                is_public: false,
                icon_id: Some(Uuid::now_v7()),
                allowed_domains: Vec::new(),
                version: 1,
                script: "return 1 + 1".to_string(),
                last_updated: crate::tools::testing::random_past_timestamp(),
                script_type: ScriptType::Scraper,
                parameters: HashMap::new(),
            },
        }
    }

    pub fn with_icon_id(mut self, icon_id: Option<Uuid>) -> Self {
        self.script.icon_id = icon_id;
        self
    }

    pub fn with_script(mut self, script: &str) -> Self {
        self.script.script = script.to_string();
        self
    }

    pub fn with_parameter(mut self, name: &str, parameter: ScriptParameter) -> Self {
        self.script.parameters.insert(name.to_string(), parameter);
        self
    }

    pub fn build(self) -> Script {
        self.script
    }
}

/// Builds a [`ScriptMetadata`] filled with random-but-plausible data, for
/// use in tests.
#[cfg(test)]
pub(crate) struct FakeScriptMetadata {
    metadata: ScriptMetadata,
}

#[cfg(test)]
impl FakeScriptMetadata {
    pub(crate) fn new() -> Self {
        Self {
            metadata: FakeScript::new().build().into(),
        }
    }

    pub(crate) fn build(self) -> ScriptMetadata {
        self.metadata
    }
}

/// Builds an [`InstalledScript`] filled with random-but-plausible data, for
/// use in tests.
#[cfg(test)]
pub(crate) struct FakeInstalledScript {
    installed: InstalledScript,
}

#[cfg(test)]
impl FakeInstalledScript {
    pub(crate) fn new() -> Self {
        Self {
            installed: InstalledScript {
                script: FakeScript::new().build(),
                parameters: HashMap::from([(
                    "username".to_string(),
                    Value::String(crate::tools::testing::random_word().to_string()),
                )]),
            },
        }
    }

    pub(crate) fn with_script(mut self, script: Script) -> Self {
        self.installed.script = script;
        self
    }

    pub(crate) fn with_parameters(mut self, parameters: HashMap<String, Value>) -> Self {
        self.installed.parameters = parameters;
        self
    }

    pub(crate) fn build(self) -> InstalledScript {
        self.installed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_script_roundtrips_through_json() {
        let installed = FakeInstalledScript::new()
            .with_script(
                FakeScript::new()
                    .with_parameter(
                        "password",
                        ScriptParameter {
                            label: "Mot de passe".to_string(),
                            placeholder: "••••••••".to_string(),
                            parameter_type: ScriptParameterType::String,
                            required: true,
                            secret: true,
                        },
                    )
                    .build(),
            )
            .build();

        let json = serde_json::to_vec(&installed).unwrap();

        assert_eq!(
            serde_json::from_slice::<InstalledScript>(&json).unwrap(),
            installed
        );
    }
}
