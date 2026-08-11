//! Agent Client Protocol bindings implemented on [`atlas_rpc`].
//!
//! The v2 surface intentionally covers ACP's session core. v1 remains available
//! for interoperating with existing agents through [`bridge`].

pub mod bridge;
pub mod host;
pub mod transcript;
pub mod v1;
pub mod v2;

/// The current ACP API version.
pub use v2 as latest;
/// ACP v2 is the default public surface. Use [`v1`] for explicit legacy support.
pub use v2::*;

use atlas_rpc::{interface, CallError, IntoHandle, Peer};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcpError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}
impl AcpError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            data: None,
        }
    }
}
impl fmt::Display for AcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    pub protocol_version: u32,
    pub info: v2::Implementation,
    #[serde(default)]
    pub capabilities: v2::Capabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u32,
    #[serde(default, alias = "agentInfo")]
    pub info: Option<v2::Implementation>,
    #[serde(default, alias = "agentCapabilities")]
    pub capabilities: v2::Capabilities,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auth_methods: Vec<serde_json::Value>,
}

#[interface]
pub trait Initializer {
    #[rpc(method = "initialize", payload)]
    async fn initialize(&self, request: InitializeRequest) -> Result<InitializeResponse, AcpError>;
}

struct ClientAdapter(v2::ClientHandle);

impl v1::Client for ClientAdapter {
    async fn session_update(
        &self,
        session_id: String,
        update: serde_json::Value,
    ) -> Result<(), AcpError> {
        self.0
            .session_update(session_id, update)
            .map_err(|error| AcpError::new(error.to_string()))
    }

    async fn request_permission(
        &self,
        session_id: String,
        title: String,
        subject: Option<String>,
        options: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, AcpError> {
        let response = self
            .0
            .request_permission(session_id, title, subject, options)
            .await
            .map_err(|error| AcpError::new(error.to_string()))?;
        serde_json::to_value(response).map_err(|error| AcpError::new(error.to_string()))
    }
}

pub async fn initialize<C>(
    peer: Peer,
    client: C,
    request: InitializeRequest,
) -> Result<v2::AgentHandle, CallError>
where
    C: v2::Client + Send + Sync + 'static,
{
    let response = InitializerHandle::new(peer.clone())
        .initialize(request)
        .await?;
    match response.protocol_version {
        v2::PROTOCOL_VERSION => {
            peer.register::<v2::ClientHandle, _>(client);
            Ok(v2::AgentHandle::new(peer))
        }
        v1::PROTOCOL_VERSION => {
            let client = client.into_handle::<v2::ClientHandle>();
            peer.register::<v1::ClientHandle, _>(ClientAdapter(client.clone()));
            Ok(bridge::BridgeV1::new(v1::AgentHandle::new(peer), client)
                .into_handle::<v2::AgentHandle>())
        }
        version => Err(CallError::Rpc(atlas_rpc::RpcError::new(
            -32602,
            format!("unsupported ACP protocol version {version}"),
        ))),
    }
}
