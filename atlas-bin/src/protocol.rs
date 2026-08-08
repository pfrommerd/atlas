//! Atlas-specific coordination RPC layered beside the ACP agent interface.

use atlas_acp::latest::SessionInfo;
use atlas_rpc::{interface, Stream};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionScope {
    Active,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListRequest {
    pub scope: SessionScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default)]
    pub deltas: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionPage {
    pub sessions: Vec<SessionInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionListEvent {
    Snapshot { sessions: Vec<SessionInfo> },
    Added { session: SessionInfo },
    Removed { session_id: String },
    Updated { session: SessionInfo },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSubscription {
    pub session_id: String,
}

#[interface]
pub trait Atlas {
    #[rpc(method = "atlas/session/list", reply_and_stream)]
    async fn list_sessions(
        &self,
        request: SessionListRequest,
    ) -> Result<(SessionPage, Stream<SessionListEvent>), atlas_acp::AcpError>;

    #[rpc(method = "atlas/session/subscribe")]
    async fn subscribe(&self, request: SessionSubscription) -> Result<(), atlas_acp::AcpError>;

    #[rpc(method = "atlas/session/unsubscribe")]
    async fn unsubscribe(&self, request: SessionSubscription) -> Result<(), atlas_acp::AcpError>;
}
