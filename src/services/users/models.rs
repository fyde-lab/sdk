use uuid::Uuid;

/// A user account, as returned by [`super::Service::create`]/[`super::Service::login`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub(super) id: Uuid,
    pub(super) username: String,
    pub(super) created_at: i64,
}

impl User {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn created_at(&self) -> i64 {
        self.created_at
    }
}

/// Builds a [`User`] filled with random-but-plausible data, overridable
/// field by field, for use in tests.
#[cfg(test)]
pub(crate) struct FakeUser {
    user: User,
}

#[cfg(test)]
impl FakeUser {
    pub(crate) fn new() -> Self {
        Self {
            user: User {
                id: Uuid::new_v4(),
                username: crate::testing::random_username(),
                created_at: crate::testing::random_past_timestamp(),
            },
        }
    }

    pub(crate) fn with_id(mut self, id: Uuid) -> Self {
        self.user.id = id;
        self
    }

    pub(crate) fn with_username(mut self, username: impl Into<String>) -> Self {
        self.user.username = username.into();
        self
    }

    pub(crate) fn with_created_at(mut self, created_at: i64) -> Self {
        self.user.created_at = created_at;
        self
    }

    pub(crate) fn build(self) -> User {
        self.user
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_user_builds_with_plausible_defaults() {
        let user = FakeUser::new().build();

        assert!(!user.username.is_empty());
    }

    #[test]
    fn fake_user_with_methods_override_defaults() {
        let id = Uuid::new_v4();

        let user = FakeUser::new()
            .with_id(id)
            .with_username("pierre")
            .with_created_at(1_700_000_000)
            .build();

        assert_eq!(user.id, id);
        assert_eq!(user.username, "pierre");
        assert_eq!(user.created_at, 1_700_000_000);
    }
}
