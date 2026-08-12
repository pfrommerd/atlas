//! V1 agent compatibility for the latest ACP session interface.

use crate::{v1, v2, AcpError};

pub struct BridgeV1 {
    agent: v1::AgentHandle,
    client: v2::ClientHandle,
}

impl BridgeV1 {
    pub fn new(agent: v1::AgentHandle, client: v2::ClientHandle) -> Self {
        Self { agent, client }
    }
}

impl v2::Agent for BridgeV1 {
    async fn new_session(
        &self,
        cwd: String,
        additional_directories: Vec<String>,
        _: Vec<serde_json::Value>,
    ) -> Result<v2::NewSessionResponse, AcpError> {
        self.agent
            .new_session(cwd, additional_directories, Vec::new())
            .await
            .map_err(|error| AcpError::new(error.to_string()))
    }

    async fn list_sessions(
        &self,
        cwd: Option<String>,
        _: Option<String>,
    ) -> Result<v2::ListSessionsResponse, AcpError> {
        self.agent
            .list_sessions(cwd, None)
            .await
            .map_err(|error| AcpError::new(error.to_string()))
    }

    async fn resume_session(
        &self,
        session_id: v2::SessionId,
        cwd: String,
        additional_directories: Vec<String>,
        _: Vec<serde_json::Value>,
        _: Option<v2::ReplayFrom>,
    ) -> Result<v2::ResumeSessionResponse, AcpError> {
        self.agent
            .resume_session(session_id, cwd, additional_directories, Vec::new())
            .await
            .map_err(|error| AcpError::new(error.to_string()))
    }

    async fn prompt(
        &self,
        _: atlas_rpc::RpcContext<v2::ClientHandle>,
        session_id: v2::SessionId,
        prompt: Vec<serde_json::Value>,
    ) -> Result<(), AcpError> {
        let response = self
            .agent
            .prompt(session_id.clone(), prompt)
            .await
            .map_err(|error| AcpError::new(error.to_string()))?;
        self.client
            .session_update(
                session_id,
                serde_json::json!({
                    "sessionUpdate": "state_update",
                    "state": "idle",
                    "stopReason": response.stop_reason,
                }),
            )
            .map_err(|error| AcpError::new(error.to_string()))
    }

    async fn cancel(&self, session_id: v2::SessionId) -> Result<(), AcpError> {
        self.agent
            .cancel(session_id)
            .map_err(|error| AcpError::new(error.to_string()))
    }

    async fn close(&self, session_id: v2::SessionId) -> Result<(), AcpError> {
        self.agent
            .close(session_id)
            .await
            .map_err(|error| AcpError::new(error.to_string()))
    }
}
