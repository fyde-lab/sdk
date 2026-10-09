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
use super::ScriptParameter;
use super::ScriptParameterType;
use super::ScriptType;

/// Generated protobuf/gRPC bindings for the `scripts` service, compiled
/// from `../api-protos/scripts/v1/scripts.proto` by `build.rs`.
mod proto {
    tonic::include_proto!("scripts.v1");
}

use proto::{
    CreateScriptRequest, DisableScriptRequest, EnableScriptRequest, FetchScriptRequest,
    ListPublicScriptsRequest, ListUserScriptsRequest, UpdateScriptRequest,
    scripts_service_client::ScriptsServiceClient as GeneratedScriptsClient,
};

/// A gRPC transport for talking to the fyde server's scripts service. Knows
/// nothing about scripts business logic beyond translating between raw
/// proto messages and the domain [`Script`] type. Abstracted as a trait so
/// callers can be tested against [`MockFydeClient`] instead of a live
/// server.
#[cfg_attr(test, automock)]
#[async_trait]
pub(super) trait FydeClient: Send + Sync {
    /// Creates a new script owned by the authenticated user, at version 1.
    #[allow(clippy::too_many_arguments)]
    async fn create_script(
        &self,
        name: &str,
        is_public: bool,
        icon: Vec<u8>,
        script: &str,
        description: &str,
        short_description: &str,
        allowed_domains: Vec<String>,
        script_type: ScriptType,
        parameters: HashMap<String, ScriptParameter>,
    ) -> Result<Script>;

    /// Fetches the script matching `id`.
    async fn fetch_script(&self, id: Uuid) -> Result<Script>;

    /// Updates a script owned by the authenticated user, incrementing its
    /// version.
    #[allow(clippy::too_many_arguments)]
    async fn update_script(
        &self,
        id: Uuid,
        name: &str,
        is_public: bool,
        icon: Vec<u8>,
        script: &str,
        description: &str,
        short_description: &str,
        allowed_domains: Vec<String>,
        script_type: ScriptType,
        parameters: HashMap<String, ScriptParameter>,
    ) -> Result<Script>;

    /// Enables `script_id` for the authenticated user.
    async fn enable_script(&self, script_id: Uuid) -> Result<()>;

    /// Disables `script_id` for the authenticated user.
    async fn disable_script(&self, script_id: Uuid) -> Result<()>;

    /// Lists the scripts currently enabled for the authenticated user.
    async fn list_user_scripts(&self) -> Result<Vec<Script>>;

    /// Lists every script marked public, regardless of who owns it or
    /// whether the authenticated user has it enabled.
    async fn list_public_scripts(&self) -> Result<Vec<Script>>;
}

/// The production [`FydeClient`] implementation, backed by a tonic
/// [`Channel`] shared with every other domain's gRPC client (see
/// [`crate::Client::init`]). Cloning a [`Channel`] is cheap — it's just a
/// handle to the same underlying connection — so a fresh generated client
/// is created per call. Every call is authenticated by attaching the
/// session token currently persisted in settings (if any) as a bearer
/// `authorization` header (see
/// [`crate::domains::sessions::Service::authenticated_request`]).
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

#[async_trait]
impl FydeClient for GrpcClient {
    async fn create_script(
        &self,
        name: &str,
        is_public: bool,
        icon: Vec<u8>,
        script: &str,
        description: &str,
        short_description: &str,
        allowed_domains: Vec<String>,
        script_type: ScriptType,
        parameters: HashMap<String, ScriptParameter>,
    ) -> Result<Script> {
        let request = self
            .sessions
            .authenticated_request(CreateScriptRequest {
                name: name.to_string(),
                is_public,
                icon,
                script: script.to_string(),
                description: description.to_string(),
                short_description: short_description.to_string(),
                allowed_domains,
                r#type: script_type.to_string(),
                parameters: parameters_into_proto(parameters),
            })
            .await?;

        let response = self
            .client()
            .create_script(request)
            .await
            .context("failed to create script")?
            .into_inner();

        into_domain(response.script.ok_or_else(|| {
            crate::Error::InvalidResponse("create script response had no script".to_string())
        })?)
    }

    async fn fetch_script(&self, id: Uuid) -> Result<Script> {
        let request = self
            .sessions
            .authenticated_request(FetchScriptRequest { id: id.to_string() })
            .await?;

        let response = self
            .client()
            .fetch_script(request)
            .await
            .context("failed to fetch script")?
            .into_inner();

        into_domain(response.script.ok_or_else(|| {
            crate::Error::InvalidResponse("fetch script response had no script".to_string())
        })?)
    }

    async fn update_script(
        &self,
        id: Uuid,
        name: &str,
        is_public: bool,
        icon: Vec<u8>,
        script: &str,
        description: &str,
        short_description: &str,
        allowed_domains: Vec<String>,
        script_type: ScriptType,
        parameters: HashMap<String, ScriptParameter>,
    ) -> Result<Script> {
        let request = self
            .sessions
            .authenticated_request(UpdateScriptRequest {
                id: id.to_string(),
                name: name.to_string(),
                is_public,
                icon,
                script: script.to_string(),
                description: description.to_string(),
                short_description: short_description.to_string(),
                allowed_domains,
                r#type: script_type.to_string(),
                parameters: parameters_into_proto(parameters),
            })
            .await?;

        let response = self
            .client()
            .update_script(request)
            .await
            .context("failed to update script")?
            .into_inner();

        into_domain(response.script.ok_or_else(|| {
            crate::Error::InvalidResponse("update script response had no script".to_string())
        })?)
    }

    async fn enable_script(&self, script_id: Uuid) -> Result<()> {
        let request = self
            .sessions
            .authenticated_request(EnableScriptRequest {
                script_id: script_id.to_string(),
            })
            .await?;

        self.client()
            .enable_script(request)
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

        self.client()
            .disable_script(request)
            .await
            .context("failed to disable script")?;

        Ok(())
    }

    async fn list_user_scripts(&self) -> Result<Vec<Script>> {
        let request = self
            .sessions
            .authenticated_request(ListUserScriptsRequest {})
            .await?;

        let response = self
            .client()
            .list_user_scripts(request)
            .await
            .context("failed to list user scripts")?
            .into_inner();

        response.scripts.into_iter().map(into_domain).collect()
    }

    async fn list_public_scripts(&self) -> Result<Vec<Script>> {
        let request = self
            .sessions
            .authenticated_request(ListPublicScriptsRequest {})
            .await?;

        let response = self
            .client()
            .list_public_scripts(request)
            .await
            .context("failed to list public scripts")?
            .into_inner();

        response.scripts.into_iter().map(into_domain).collect()
    }
}

#[cfg(test)]
mod tests {
    use tonic::transport::Endpoint;

    use super::*;
    use crate::Error;
    use crate::domains::settings::MockService as MockSettingsService;

    fn sessions() -> Arc<SessionsClient> {
        let mut settings = MockSettingsService::new();
        settings.expect_get().returning(|_| Ok(None));
        Arc::new(SessionsClient::new(Arc::new(settings)))
    }

    #[tokio::test]
    async fn new_does_not_connect_to_the_server() {
        // A syntactically valid but unreachable address, connected lazily:
        // if `client()` dialed eagerly at construction, this would fail
        // here rather than on first use below.
        let channel = Endpoint::from_static("http://127.0.0.1:1").connect_lazy();
        let grpc = GrpcClient::new(channel, sessions());

        let err = grpc.list_user_scripts().await.unwrap_err();

        assert!(matches!(err, Error::Context { .. }));
    }
}
