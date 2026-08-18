//! ACP v1 compatibility surface used by the bridge.

use crate::{AcpError, v2};
use atlas_rpc::{RpcContext, interface};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub const PROTOCOL_VERSION: u32 = 1;

pub use crate::{InitializeRequest, InitializeResponse};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub stop_reason: String,
}

#[interface]
pub trait Client {
    #[rpc(method = "session/update", notification)]
    async fn session_update(
        &self,
        #[serde(rename = "sessionId")] session_id: String,
        update: Value,
    ) -> Result<(), AcpError>;
    #[rpc(method = "session/request_permission")]
    async fn request_permission(
        &self,
        #[serde(rename = "sessionId")] session_id: String,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")] subject: Option<String>,
        #[serde(default)] options: Vec<Value>,
    ) -> Result<Value, AcpError>;
}

#[interface]
pub trait Agent {
    #[rpc(method = "session/new")]
    async fn new_session(
        &self,
        cwd: String,
        #[serde(
            rename = "additionalDirectories",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        additional_directories: Vec<String>,
        #[serde(rename = "mcpServers", default)] mcp_servers: Vec<Value>,
    ) -> Result<v2::NewSessionResponse, AcpError>;
    #[rpc(method = "session/list")]
    async fn list_sessions(
        &self,
        #[serde(skip_serializing_if = "Option::is_none")] cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")] cursor: Option<String>,
    ) -> Result<v2::ListSessionsResponse, AcpError>;
    #[rpc(method = "session/load")]
    async fn load_session(
        &self,
        #[serde(rename = "sessionId")] session_id: String,
        cwd: String,
        #[serde(rename = "mcpServers", default)] mcp_servers: Vec<Value>,
    ) -> Result<v2::ResumeSessionResponse, AcpError>;
    #[rpc(method = "session/resume")]
    async fn resume_session(
        &self,
        #[serde(rename = "sessionId")] session_id: String,
        cwd: String,
        #[serde(
            rename = "additionalDirectories",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        additional_directories: Vec<String>,
        #[serde(rename = "mcpServers", default)] mcp_servers: Vec<Value>,
    ) -> Result<v2::ResumeSessionResponse, AcpError>;
    #[rpc(method = "session/prompt", ordered)]
    async fn prompt(
        &self,
        #[rpc(context)] client: RpcContext<ClientHandle>,
        #[serde(rename = "sessionId")] session_id: String,
        prompt: Vec<Value>,
    ) -> Result<PromptResponse, AcpError>;
    #[rpc(method = "session/cancel", notification)]
    async fn cancel(
        &self,
        #[serde(rename = "sessionId")] session_id: String,
    ) -> Result<(), AcpError>;
    #[rpc(method = "session/close")]
    async fn close(
        &self,
        #[serde(rename = "sessionId")] session_id: String,
    ) -> Result<(), AcpError>;
}
