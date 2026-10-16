use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;
use tonic::transport::Channel;
use uuid::Uuid;

use crate::domains::sessions::{Service as SessionsService, SessionsClient};
use crate::{ErrorContext as _, Result};

use super::Script;
use super::ScriptMetadata;
use super::ScriptParameter;
use super::ScriptParameterType;
use super::ScriptType;

/// Generated protobuf/gRPC bindings for the `scripts` service, compiled by
/// `build.rs` from `scripts/v1/scripts.proto` in the `buf.build/fyde-lab/api`
/// BSR module (sourced from `../server/protos`).
mod proto {
    tonic::include_proto!("scripts.v1");
}

use proto::{
    CreateScriptRequest, DisableScriptRequest, EnableScriptRequest, FetchScriptRequest,
    ListPublicScriptsRequest, ListUserScriptsRequest, UpdateScriptRequest,
    scripts_service_client::ScriptsServiceClient as GeneratedScriptsClient,
};

/// Every field a script is created or updated with, by name rather than as
/// a long run of positional arguments — several of them are `String`s, so
/// swapping two (say `description` and `short_description`) at a call site
/// would otherwise still compile.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ScriptDraft {
    pub(super) name: String,
    pub(super) is_public: bool,
    pub(super) icon: Vec<u8>,
    pub(super) script: String,
    pub(super) description: String,
    pub(super) short_description: String,
    pub(super) allowed_domains: Vec<String>,
    pub(super) script_type: ScriptType,
    pub(super) parameters: HashMap<String, ScriptParameter>,
}

/// One page of one of the server's paginated script listings.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ScriptMetadataPage {
    pub(super) scripts: Vec<ScriptMetadata>,
    /// The token listing the page after this one, or `None` when this is
    /// the last page.
    pub(super) next_page_token: Option<String>,
}

/// A gRPC transport for talking to the fyde server's scripts service. Knows
/// nothing about scripts business logic beyond translating between raw
/// proto messages and the domain [`Script`] type. Abstracted as a trait so
/// callers can be tested against [`MockFydeClient`] instead of a live
/// server.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// Creates a new script owned by the authenticated user, at version 1.
    async fn create_script(&self, draft: ScriptDraft) -> Result<Script>;

    /// Fetches the script matching `id`.
    async fn fetch_script(&self, id: Uuid) -> Result<Script>;

    /// Updates a script owned by the authenticated user, incrementing its
    /// version.
    async fn update_script(&self, id: Uuid, draft: ScriptDraft) -> Result<Script>;

    /// Enables `script_id` for the authenticated user.
    async fn enable_script(&self, script_id: Uuid) -> Result<()>;

    /// Disables `script_id` for the authenticated user.
    async fn disable_script(&self, script_id: Uuid) -> Result<()>;

    /// Lists one page of the metadata of the scripts currently enabled for
    /// the authenticated user: the first one for a `None` `page_token`,
    /// otherwise the one after the page that returned it. The page size is
    /// left to the server's default.
    async fn list_user_scripts(&self, page_token: Option<String>) -> Result<ScriptMetadataPage>;

    /// Lists one page of the metadata of every script marked public,
    /// regardless of who owns it or whether the authenticated user has it
    /// enabled — see [`Self::list_user_scripts`] for `page_token`.
    async fn list_public_scripts(&self, page_token: Option<String>) -> Result<ScriptMetadataPage>;
}

/// The production [`FydeClient`] implementation, backed by a tonic
/// [`Channel`] shared with every other domain's gRPC client (see
/// [`crate::Client::init`]). Cloning a [`Channel`] is cheap — it's just a
/// handle to the same underlying connection — so a fresh generated client
/// is created per call. Every call is authenticated by attaching the
/// session token currently persisted in the secrets store (if any) as a bearer
/// `authorization` header (see
/// [`crate::domains::sessions::Service::authenticated_request`]), and its
/// outcome checked for a session the server rejected (see
/// [`crate::domains::sessions::Service::check_response`]).
pub(super) struct GrpcClient {
    channel: Channel,
    sessions: Arc<SessionsClient>,
}

impl GrpcClient {
    /// Creates a client for the scripts service using the shared `channel`
    /// connection to the fyde server, authenticating every call via
    /// `sessions`.
    pub fn new(channel: Channel, sessions: Arc<SessionsClient>) -> Self {
        Self { channel, sessions }
    }

    /// Returns a generated client wrapping the shared connection.
    fn client(&self) -> GeneratedScriptsClient<Channel> {
        GeneratedScriptsClient::new(self.channel.clone())
    }
}

#[async_trait]
impl FydeClient for GrpcClient {
    async fn create_script(&self, draft: ScriptDraft) -> Result<Script> {
        let request = self
            .sessions
            .authenticated_request(CreateScriptRequest {
                name: draft.name,
                is_public: draft.is_public,
                icon: draft.icon,
                script: draft.script,
                description: draft.description,
                short_description: draft.short_description,
                allowed_domains: draft.allowed_domains,
                r#type: draft.script_type.to_string(),
                parameters: parameters_into_proto(draft.parameters),
            })
            .await?;

        let response = self
            .sessions
            .check_response(self.client().create_script(request).await)
            .await
            .context("failed to create script")?
            .into_inner();

        into_domain(response.script.ok_or_else(|| {
            crate::ErrorKind::InvalidResponse("create script response had no script".to_string())
        })?)
    }

    async fn fetch_script(&self, id: Uuid) -> Result<Script> {
        let request = self
            .sessions
            .authenticated_request(FetchScriptRequest { id: id.to_string() })
            .await?;

        let response = self
            .sessions
            .check_response(self.client().fetch_script(request).await)
            .await
            .context("failed to fetch script")?
            .into_inner();

        into_domain(response.script.ok_or_else(|| {
            crate::ErrorKind::InvalidResponse("fetch script response had no script".to_string())
        })?)
    }

    async fn update_script(&self, id: Uuid, draft: ScriptDraft) -> Result<Script> {
        let request = self
            .sessions
            .authenticated_request(UpdateScriptRequest {
                id: id.to_string(),
                name: draft.name,
                is_public: draft.is_public,
                icon: draft.icon,
                script: draft.script,
                description: draft.description,
                short_description: draft.short_description,
                allowed_domains: draft.allowed_domains,
                r#type: draft.script_type.to_string(),
                parameters: parameters_into_proto(draft.parameters),
            })
            .await?;

        let response = self
            .sessions
            .check_response(self.client().update_script(request).await)
            .await
            .context("failed to update script")?
            .into_inner();

        into_domain(response.script.ok_or_else(|| {
            crate::ErrorKind::InvalidResponse("update script response had no script".to_string())
        })?)
    }

    async fn enable_script(&self, script_id: Uuid) -> Result<()> {
        let request = self
            .sessions
            .authenticated_request(EnableScriptRequest {
                script_id: script_id.to_string(),
            })
            .await?;

        self.sessions
            .check_response(self.client().enable_script(request).await)
            .await
            .context("failed to enable script")?;

        Ok(())
    }

    async fn disable_script(&self, script_id: Uuid) -> Result<()> {
        let request = self
            .sessions
            .authenticated_request(DisableScriptRequest {
                script_id: script_id.to_string(),
            })
            .await?;

        self.sessions
            .check_response(self.client().disable_script(request).await)
            .await
            .context("failed to disable script")?;

        Ok(())
    }

    async fn list_user_scripts(&self, page_token: Option<String>) -> Result<ScriptMetadataPage> {
        let request = self
            .sessions
            .authenticated_request(ListUserScriptsRequest {
                page_size: 0,
                page_token: page_token.unwrap_or_default(),
            })
            .await?;

        let response = self
            .sessions
            .check_response(self.client().list_user_scripts(request).await)
            .await
            .context("failed to list user scripts")?
            .into_inner();

        page_into_domain(response.scripts, response.next_page_token)
    }

    async fn list_public_scripts(&self, page_token: Option<String>) -> Result<ScriptMetadataPage> {
        let request = self
            .sessions
            .authenticated_request(ListPublicScriptsRequest {
                page_size: 0,
                page_token: page_token.unwrap_or_default(),
            })
            .await?;

        let response = self
            .sessions
            .check_response(self.client().list_public_scripts(request).await)
            .await
            .context("failed to list public scripts")?
            .into_inner();

        page_into_domain(response.scripts, response.next_page_token)
    }
}

/// Converts a generated `proto::script_parameter::Type` into the domain
/// [`ScriptParameterType`]. Unknown/unspecified values fall back to
/// [`ScriptParameterType::String`] rather than failing the whole script,
/// since this is just a form-rendering hint.
fn parameter_type_into_domain(parameter_type: i32) -> ScriptParameterType {
    match proto::script_parameter::Type::try_from(parameter_type) {
        Ok(proto::script_parameter::Type::Number) => ScriptParameterType::Number,
        Ok(proto::script_parameter::Type::Boolean) => ScriptParameterType::Boolean,
        _ => ScriptParameterType::String,
    }
}

fn parameter_type_into_proto(parameter_type: ScriptParameterType) -> i32 {
    match parameter_type {
        ScriptParameterType::String => proto::script_parameter::Type::String as i32,
        ScriptParameterType::Number => proto::script_parameter::Type::Number as i32,
        ScriptParameterType::Boolean => proto::script_parameter::Type::Boolean as i32,
    }
}

fn parameters_into_domain(
    parameters: HashMap<String, proto::ScriptParameter>,
) -> HashMap<String, ScriptParameter> {
    parameters
        .into_iter()
        .map(|(key, parameter)| {
            (
                key,
                ScriptParameter {
                    label: parameter.label,
                    placeholder: parameter.placeholder,
                    parameter_type: parameter_type_into_domain(parameter.r#type),
                    required: parameter.required,
                    secret: parameter.secret,
                },
            )
        })
        .collect()
}

fn parameters_into_proto(
    parameters: HashMap<String, ScriptParameter>,
) -> HashMap<String, proto::ScriptParameter> {
    parameters
        .into_iter()
        .map(|(key, parameter)| {
            (
                key,
                proto::ScriptParameter {
                    label: parameter.label,
                    placeholder: parameter.placeholder,
                    r#type: parameter_type_into_proto(parameter.parameter_type),
                    required: parameter.required,
                    secret: parameter.secret,
                },
            )
        })
        .collect()
}

/// Converts a generated `proto::Script` into the domain [`Script`] type,
/// parsing its canonical UUIDv7 string id and its `type` string.
fn into_domain(script: proto::Script) -> Result<Script> {
    Ok(Script {
        id: script
            .id
            .parse()
            .context("failed to parse script id returned by the server")?,
        name: script.name,
        description: script.description,
        short_description: script.short_description,
        is_public: script.is_public,
        icon: script.icon,
        allowed_domains: script.allowed_domains,
        version: script.version,
        script: script.script,
        last_updated: script.last_updated,
        script_type: script
            .r#type
            .parse()
            .context("failed to parse script type returned by the server")?,
        parameters: parameters_into_domain(script.parameters),
    })
}

/// Converts a listing response's generated `proto::ScriptMetadata`s and
/// `next_page_token` (empty on the last page) into a domain
/// [`ScriptMetadataPage`].
fn page_into_domain(
    scripts: Vec<proto::ScriptMetadata>,
    next_page_token: String,
) -> Result<ScriptMetadataPage> {
    Ok(ScriptMetadataPage {
        scripts: scripts
            .into_iter()
            .map(metadata_into_domain)
            .collect::<Result<_>>()?,
        next_page_token: (!next_page_token.is_empty()).then_some(next_page_token),
    })
}

/// Converts a generated `proto::ScriptMetadata` into the domain
/// [`ScriptMetadata`] type, parsing it like [`into_domain`] does a script.
fn metadata_into_domain(metadata: proto::ScriptMetadata) -> Result<ScriptMetadata> {
    Ok(ScriptMetadata {
        id: metadata
            .id
            .parse()
            .context("failed to parse script id returned by the server")?,
        name: metadata.name,
        description: metadata.description,
        short_description: metadata.short_description,
        is_public: metadata.is_public,
        icon: metadata.icon,
        allowed_domains: metadata.allowed_domains,
        version: metadata.version,
        last_updated: metadata.last_updated,
        script_type: metadata
            .r#type
            .parse()
            .context("failed to parse script type returned by the server")?,
        parameters: parameters_into_domain(metadata.parameters),
    })
}

#[cfg(test)]
mod tests {
    use tonic::transport::Endpoint;

    use super::*;
    use crate::domains::secrets::MockService as MockSecretsService;

    fn sessions() -> Arc<SessionsClient> {
        let mut secrets = MockSecretsService::new();
        secrets.expect_get().returning(|_| Ok(None));
        Arc::new(SessionsClient::new(Arc::new(secrets)))
    }

    #[tokio::test]
    async fn new_does_not_connect_to_the_server() {
        // A syntactically valid but unreachable address, connected lazily:
        // if `client()` dialed eagerly at construction, this would fail
        // here rather than on first use below.
        let channel = Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let grpc = GrpcClient::new(channel, sessions());

        let err = grpc.list_user_scripts(None).await.unwrap_err();

        assert!(err.has_context());
    }
}
