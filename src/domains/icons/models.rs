use uuid::Uuid;

/// An image shown for another entity (e.g. a script's icon), as returned by
/// [`super::Service::create_icon`]/[`super::Service::fetch_icon`]. Never
/// changes once created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Icon {
    pub(super) id: Uuid,
    pub(super) content: Vec<u8>,
}

impl Icon {
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// The encoded image (e.g. a PNG).
    pub fn content(&self) -> &[u8] {
        &self.content
    }
}

/// Builds an [`Icon`] filled with random-but-plausible data, overridable
/// field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeIcon {
    icon: Icon,
}

#[cfg(test)]
impl FakeIcon {
    pub(crate) fn new() -> Self {
        Self {
            icon: Icon {
                id: Uuid::new_v4(),
                content: crate::tools::testing::random_bytes(32),
            },
        }
    }

    pub(crate) fn with_id(mut self, id: Uuid) -> Self {
        self.icon.id = id;
        self
    }

    pub(crate) fn with_content(mut self, content: Vec<u8>) -> Self {
        self.icon.content = content;
        self
    }

    pub(crate) fn build(self) -> Icon {
        self.icon
    }
}
