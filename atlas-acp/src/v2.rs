use crate::AcpError;
use atlas_rpc::{interface, RpcContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 2;
pub type SessionId = String;
pub type Meta = Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Implementation {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Capabilities {
    #[serde(default)]
    pub session: Option<Value>,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, Value>,
}
pub use crate::{InitializeRequest, InitializeResponse};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: SessionId,
    #[serde(default)]
    pub config_options: Vec<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: SessionId,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<Meta>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSessionsResponse {
    pub sessions: Vec<SessionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ResumeSessionResponse {}

/// Inclusive cursor describing where a resumed session should begin replaying.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplayFrom {
    /// Replay the entire conversation before accepting new work.
    Start,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdate {
    pub session_id: SessionId,
    #[serde(flatten)]
    pub update: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResponse {
    pub outcome: Value,
}

#[interface]
pub trait Client {
    #[rpc(method = "session/update", notification)]
    async fn session_update(
        &self,
        #[serde(rename = "sessionId")] session_id: SessionId,
        update: Value,
    ) -> Result<(), AcpError>;
    #[rpc(method = "session/request_permission")]
    async fn request_permission(
        &self,
        #[serde(rename = "sessionId")] session_id: SessionId,
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")] subject: Option<String>,
        #[serde(default)] options: Vec<Value>,
    ) -> Result<PermissionResponse, AcpError>;
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
    ) -> Result<NewSessionResponse, AcpError>;
    #[rpc(method = "session/list")]
    async fn list_sessions(
        &self,
        #[serde(skip_serializing_if = "Option::is_none")] cwd: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")] cursor: Option<String>,
    ) -> Result<ListSessionsResponse, AcpError>;
    #[rpc(method = "session/resume")]
    async fn resume_session(
        &self,
        #[serde(rename = "sessionId")] session_id: SessionId,
        cwd: String,
        #[serde(
            rename = "additionalDirectories",
            default,
            skip_serializing_if = "Vec::is_empty"
        )]
        additional_directories: Vec<String>,
        #[serde(rename = "mcpServers", default)] mcp_servers: Vec<Value>,
        #[serde(
            rename = "replayFrom",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        replay_from: Option<ReplayFrom>,
    ) -> Result<ResumeSessionResponse, AcpError>;
    #[rpc(method = "session/prompt")]
    async fn prompt(
        &self,
        #[rpc(context)] client: RpcContext<ClientHandle>,
        #[serde(rename = "sessionId")] session_id: SessionId,
        prompt: Vec<Value>,
    ) -> Result<(), AcpError>;
    #[rpc(method = "session/cancel", notification)]
    async fn cancel(
        &self,
        #[serde(rename = "sessionId")] session_id: SessionId,
    ) -> Result<(), AcpError>;
    #[rpc(method = "session/close")]
    async fn close(
        &self,
        #[serde(rename = "sessionId")] session_id: SessionId,
    ) -> Result<(), AcpError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replay_from_start_uses_the_acp_wire_shape() {
        assert_eq!(serde_json::to_value(ReplayFrom::Start).unwrap(), json!({"type": "start"}));
    }
}
