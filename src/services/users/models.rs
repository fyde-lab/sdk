use uuid::Uuid;

/// A user account, as returned by [`super::Service::create`]/[`super::Service::login`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub created_at: i64,
}
