use uuid::Uuid;

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
    pub(super) logo: Vec<u8>,
    pub(super) version: u64,
    pub(super) script: String,
    /// Unix timestamp, in seconds, of the last time this script was
    /// updated.
    pub(super) last_updated: i64,
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

    /// The script's marketplace banner/logo image — distinct from
    /// [`Self::icon`] (its small UI icon).
    pub fn logo(&self) -> &[u8] {
        &self.logo
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
                logo: crate::testing::random_bytes(16),
                version: 1,
                script: "return 1 + 1".to_string(),
                last_updated: crate::testing::random_past_timestamp(),
            },
        }
    }

    pub(crate) fn with_script(mut self, script: &str) -> Self {
        self.script.script = script.to_string();
        self
    }

    pub(crate) fn build(self) -> Script {
        self.script
    }
}
