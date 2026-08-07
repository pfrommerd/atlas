//! Atlas-specific coordination RPC layered beside the ACP agent interface.

use atlas_acp::latest::SessionInfo;
use atlas_rpc::{interface, Stream};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionFilter {
    Active,
    All,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListRequest {
    pub filter: SessionFilter,
    #[serde(default)]
    pub deltas: bool,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Empty {}

#[interface]
pub trait Atlas {
    #[rpc(method = "atlas/session/list", reply_and_stream)]
    async fn list_sessions(
        &self,
        request: SessionListRequest,
    ) -> Result<(Vec<SessionInfo>, Stream<SessionListEvent>), atlas_acp::AcpError>;

    #[rpc(method = "atlas/session/subscribe")]
    async fn subscribe(&self, request: SessionSubscription) -> Result<Empty, atlas_acp::AcpError>;

    #[rpc(method = "atlas/session/unsubscribe")]
    async fn unsubscribe(&self, request: SessionSubscription)
        -> Result<Empty, atlas_acp::AcpError>;
}
