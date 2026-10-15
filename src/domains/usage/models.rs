/// The plan an account is on, and what the account currently uses of each
/// of its limits. Every figure is shared by all the account's users and
/// devices. A usage figure may exceed its limit after the account moved to
/// a smaller plan: what it already stores is kept, and only further writes
/// are refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    pub(super) plan_id: String,
    pub(super) max_scripts: u32,
    pub(super) max_sessions: u32,
    pub(super) max_size: u64,
    pub(super) enabled_scripts: u32,
    pub(super) open_sessions: u32,
    pub(super) total_size: u64,
}

impl Usage {
    /// A stable, human-readable identifier of the plan, e.g. `"free"`.
    pub fn plan_id(&self) -> &str {
        &self.plan_id
    }

    /// How many scripts the account may have enabled at once.
    pub fn max_scripts(&self) -> u32 {
        self.max_scripts
    }

    /// How many sessions (logged-in devices) the account may have open at
    /// once.
    pub fn max_sessions(&self) -> u32 {
        self.max_sessions
    }

    /// How many bytes the account may store.
    pub fn max_size(&self) -> u64 {
        self.max_size
    }

    /// How many scripts the account currently has enabled.
    pub fn enabled_scripts(&self) -> u32 {
        self.enabled_scripts
    }

    /// How many sessions the account currently has open, this one
    /// included.
    pub fn open_sessions(&self) -> u32 {
        self.open_sessions
    }

    /// How many bytes the account currently stores.
    pub fn total_size(&self) -> u64 {
        self.total_size
    }
}

/// Builds a [`Usage`] filled with random-but-plausible data, overridable
/// field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeUsage {
    usage: Usage,
}

#[cfg(test)]
impl FakeUsage {
    pub(crate) fn new() -> Self {
        use crate::tools::testing::{random_u64, random_word};

        Self {
            usage: Usage {
                plan_id: random_word().to_string(),
                max_scripts: random_u64(50) as u32 + 1,
                max_sessions: random_u64(10) as u32 + 1,
                max_size: random_u64(1 << 30) + 1,
                enabled_scripts: random_u64(50) as u32,
                open_sessions: random_u64(10) as u32 + 1,
                total_size: random_u64(1 << 30),
            },
        }
    }

    pub(crate) fn with_plan_id(mut self, plan_id: &str) -> Self {
        self.usage.plan_id = plan_id.to_string();
        self
    }

    pub(crate) fn with_total_size(mut self, total_size: u64) -> Self {
        self.usage.total_size = total_size;
        self
    }

    pub(crate) fn build(self) -> Usage {
        self.usage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_methods_override_defaults() {
        let usage = FakeUsage::new()
            .with_plan_id("basic")
            .with_total_size(42)
            .build();

        assert_eq!(usage.plan_id(), "basic");
        assert_eq!(usage.total_size(), 42);
    }
}
