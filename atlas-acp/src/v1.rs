//! ACP v1 compatibility surface used by the bridge.

use crate::AcpError;
use atlas_rpc::{interface, RpcContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeRequest {
    pub protocol_version: u32,
    #[serde(default)]
    pub client_capabilities: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_info: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u32,
    #[serde(default)]
    pub agent_capabilities: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_info: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptRequest {
    pub session_id: String,
    pub prompt: Vec<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub stop_reason: String,
}

#[interface]
pub trait Client {
    #[rpc(method = "session/update", notification)]
    async fn session_update(&self, update: Value) -> Result<(), AcpError>;
    #[rpc(method = "session/request_permission")]
    async fn request_permission(&self, request: Value) -> Result<Value, AcpError>;
}

#[interface]
pub trait Agent {
    #[rpc(method = "initialize")]
    async fn initialize(&self, request: InitializeRequest) -> Result<InitializeResponse, AcpError>;
    #[rpc(method = "session/prompt")]
    async fn prompt(
        &self,
        #[rpc(context)] client: RpcContext<ClientClient>,
        request: PromptRequest,
    ) -> Result<PromptResponse, AcpError>;
    #[rpc(method = "session/cancel", notification)]
    async fn cancel(&self, request: Value) -> Result<(), AcpError>;
}
