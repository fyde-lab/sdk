//! A sub-domain of `documents` owning everything about document attributes:
//! their typed values ([`Attribute`], with the Lua conversions scripts use
//! in [`lua`]), the definitions those values are checked against
//! ([`AttributeDefinition`], kept on the fyde server: the shared defaults
//! plus each user's own), and the checks themselves.

mod grpc_client;
pub(crate) mod lua;
mod models;
mod service;

#[cfg(test)]
pub(crate) use models::FakeAttributeDefinition;
pub use models::{Attribute, AttributeDefinition, AttributeType, SetAttributeRequest};
pub(crate) use service::check_values;

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;

use crate::Result;
use crate::domains::sessions::SessionsClient;

/// Manages the definitions of the attributes documents can carry, and
/// checks attribute values against them.
#[cfg_attr(test, automock)]
#[async_trait]
pub trait Service: Send + Sync {
    /// Lists every attribute definition the signed-in user can use: the
    /// default ones, shared by every user, then its own, each group by name.
    async fn list(&self) -> Result<Vec<AttributeDefinition>>;

    /// Defines an attribute for the signed-in user, or redefines (type and
    /// description) the one it already has under `request.name`. Fails with
    /// [`crate::ErrorCode::InvalidAttribute`] for a name that isn't
    /// `snake_case` or a description longer than 1024 bytes; the server
    /// also refuses a default attribute's name and more than 100
    /// attributes of the user's own.
    async fn set(&self, request: SetAttributeRequest) -> Result<AttributeDefinition>;

    /// Checks `attributes` can be stored on a document, failing with
    /// [`crate::ErrorCode::InvalidAttribute`] for one that isn't valid on its
    /// own (see `service::check_values`: names, sizes, dates, currencies,
    /// selects...) or whose type isn't the one its definition — among
    /// [`Self::list`]'s — expects. An attribute nothing defines is accepted
    /// as is. Only lists definitions from the server when there's an
    /// attribute to check.
    async fn check(&self, attributes: &BTreeMap<String, Attribute>) -> Result<()>;
}

/// Initializes the attributes [`Service`] over the shared `channel` to the
/// fyde server, authenticating every call via `sessions`.
pub(crate) fn init(channel: Channel, sessions: Arc<SessionsClient>) -> Arc<dyn Service> {
    Arc::new(service::AttributesClient::new(
        grpc_client::GrpcClient::new(channel, sessions),
    ))
}
