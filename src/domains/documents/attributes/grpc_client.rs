use std::str::FromStr as _;
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::domains::sessions::{Service as SessionsService, SessionsClient};
use crate::{ErrorContext as _, ErrorKind, Result};

use super::{AttributeDefinition, AttributeType, SetAttributeRequest};

/// Generated protobuf/gRPC bindings for the `attributes` service, compiled
/// by `build.rs` from `attributes/v1/attributes.proto` in the
/// `buf.build/fyde-lab/api` BSR module (sourced from `../server/protos`).
mod proto {
    tonic::include_proto!("attributes.v1");
}

use proto::{
    ListAttributesRequest, SetAttributeRequest as ProtoSetAttributeRequest,
    attribute::Type as ProtoType,
    attributes_service_client::AttributesServiceClient as GeneratedAttributesClient,
};

/// A gRPC transport for talking to the fyde server's attributes service.
/// Knows nothing about attributes beyond translating between raw proto
/// messages and [`AttributeDefinition`]. Abstracted as a trait so callers
/// can be tested against [`MockFydeClient`] instead of a live server.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// Lists every attribute definition the authenticated user can use.
    async fn list_attributes(&self) -> Result<Vec<AttributeDefinition>>;

    /// Defines (or redefines) an attribute for the authenticated user.
    async fn set_attribute(&self, request: SetAttributeRequest) -> Result<AttributeDefinition>;
}

/// The production [`FydeClient`] implementation, backed by a tonic
/// [`Channel`] shared with every other domain's gRPC client (see
/// [`crate::Client::init`]). Every call is authenticated with the persisted
/// session token (see
/// [`crate::domains::sessions::Service::authenticated_request`]), and its
/// outcome checked for a session the server rejected (see
/// [`crate::domains::sessions::Service::check_response`]).
pub(super) struct GrpcClient {
    channel: Channel,
    sessions: Arc<SessionsClient>,
}

impl GrpcClient {
    pub(super) fn new(channel: Channel, sessions: Arc<SessionsClient>) -> Self {
        Self { channel, sessions }
    }

    /// Returns a generated client wrapping the shared connection.
    fn client(&self) -> GeneratedAttributesClient<Channel> {
        GeneratedAttributesClient::new(self.channel.clone())
    }
}

#[async_trait]
impl FydeClient for GrpcClient {
    async fn list_attributes(&self) -> Result<Vec<AttributeDefinition>> {
        let request = self
            .sessions
            .authenticated_request(ListAttributesRequest {})
            .await?;

        let response = self
            .sessions
            .check_response(self.client().list_attributes(request).await)
            .await
            .context("failed to list attributes")?
            .into_inner();

        response.attributes.into_iter().map(into_domain).collect()
    }

    async fn set_attribute(&self, request: SetAttributeRequest) -> Result<AttributeDefinition> {
        let request = self
            .sessions
            .authenticated_request(ProtoSetAttributeRequest {
                name: request.name,
                r#type: type_into_proto(request.r#type) as i32,
                description: request.description,
            })
            .await?;

        let response = self
            .sessions
            .check_response(self.client().set_attribute(request).await)
            .await
            .context("failed to set attribute")?
            .into_inner();

        into_domain(response.attribute.ok_or_else(|| {
            ErrorKind::InvalidResponse("set attribute response had no attribute".to_string())
        })?)
    }
}

fn type_into_proto(r#type: AttributeType) -> ProtoType {
    match r#type {
        AttributeType::String => ProtoType::String,
        AttributeType::Integer => ProtoType::Integer,
        AttributeType::Float => ProtoType::Float,
        AttributeType::Boolean => ProtoType::Boolean,
        AttributeType::Date => ProtoType::Date,
        AttributeType::Amount => ProtoType::Amount,
        AttributeType::Select => ProtoType::Select,
    }
}

fn type_into_domain(r#type: i32) -> Result<AttributeType> {
    match ProtoType::try_from(r#type) {
        Ok(ProtoType::String) => Ok(AttributeType::String),
        Ok(ProtoType::Integer) => Ok(AttributeType::Integer),
        Ok(ProtoType::Float) => Ok(AttributeType::Float),
        Ok(ProtoType::Boolean) => Ok(AttributeType::Boolean),
        Ok(ProtoType::Date) => Ok(AttributeType::Date),
        Ok(ProtoType::Amount) => Ok(AttributeType::Amount),
        Ok(ProtoType::Select) => Ok(AttributeType::Select),
        Ok(ProtoType::Unspecified) | Err(_) => Err(ErrorKind::InvalidResponse(format!(
            "server sent an attribute of unknown type {type}"
        ))
        .into()),
    }
}

fn into_domain(attribute: proto::Attribute) -> Result<AttributeDefinition> {
    Ok(AttributeDefinition {
        id: Uuid::from_str(&attribute.id)
            .with_context(|| format!("server sent an invalid attribute id {:?}", attribute.id))?,
        r#type: type_into_domain(attribute.r#type)?,
        name: attribute.name,
        description: attribute.description,
        is_default: attribute.is_default,
    })
}
