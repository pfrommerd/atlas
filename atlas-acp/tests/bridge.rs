use std::sync::{Arc, Mutex};

use atlas_acp::{
    AcpError, InitializeRequest, InitializeResponse, Initializer, InitializerHandle, initialize,
    v1, v2,
};
use atlas_rpc::{InProcessTransport, Peer, RpcContext};

struct LegacyInitializer;
impl Initializer for LegacyInitializer {
    async fn initialize(&self, _: InitializeRequest) -> Result<InitializeResponse, AcpError> {
        Ok(InitializeResponse {
            protocol_version: v1::PROTOCOL_VERSION,
            info: Some(v2::Implementation {
                name: "legacy".into(),
                title: "Legacy".into(),
                version: None,
            }),
            capabilities: v2::Capabilities::default(),
            auth_methods: Vec::new(),
        })
    }
}

#[derive(Clone, Default)]
struct LegacyAgent {
    calls: Arc<Mutex<Vec<&'static str>>>,
}
impl v1::Agent for LegacyAgent {
    async fn new_session(
        &self,
        _: String,
        _: Vec<String>,
        _: Vec<serde_json::Value>,
    ) -> Result<v2::NewSessionResponse, AcpError> {
        Ok(v2::NewSessionResponse {
            session_id: "legacy-session".into(),
            config_options: Vec::new(),
        })
    }

    async fn list_sessions(
        &self,
        _: Option<String>,
        _: Option<String>,
    ) -> Result<v2::ListSessionsResponse, AcpError> {
        Ok(v2::ListSessionsResponse {
            sessions: vec![v2::SessionInfo {
                session_id: "legacy-session".into(),
                cwd: "/workspace".into(),
                additional_directories: Vec::new(),
                title: Some("Remembered title".into()),
                updated_at: None,
                meta: None,
            }],
            next_cursor: None,
        })
    }

    async fn resume_session(
        &self,
        _: String,
        _: String,
        _: Vec<String>,
        _: Vec<serde_json::Value>,
    ) -> Result<v2::ResumeSessionResponse, AcpError> {
        self.calls.lock().unwrap().push("resume");
        Ok(v2::ResumeSessionResponse {})
    }

    async fn load_session(
        &self,
        _: String,
        _: String,
        _: Vec<serde_json::Value>,
    ) -> Result<v2::ResumeSessionResponse, AcpError> {
        self.calls.lock().unwrap().push("load");
        Ok(v2::ResumeSessionResponse {})
    }

    async fn prompt(
        &self,
        client: RpcContext<v1::ClientHandle>,
        session_id: String,
        _: Vec<serde_json::Value>,
    ) -> Result<v1::PromptResponse, AcpError> {
        client
            .handle()
            .session_update(
                session_id,
                serde_json::json!({"sessionUpdate": "agent_message_chunk"}),
            )
            .map_err(|error| AcpError::new(error.to_string()))?;
        Ok(v1::PromptResponse {
            stop_reason: "end_turn".into(),
        })
    }

    async fn cancel(&self, _: String) -> Result<(), AcpError> {
        Ok(())
    }

    async fn close(&self, _: String) -> Result<(), AcpError> {
        Ok(())
    }
}

#[derive(Clone)]
struct RecordingClient(Arc<Mutex<Vec<(String, serde_json::Value)>>>);
impl v2::Client for RecordingClient {
    async fn session_update(
        &self,
        session_id: String,
        update: serde_json::Value,
    ) -> Result<(), AcpError> {
        self.0.lock().unwrap().push((session_id, update));
        Ok(())
    }

    async fn request_permission(
        &self,
        _: String,
        _: String,
        _: Option<String>,
        _: Vec<serde_json::Value>,
    ) -> Result<v2::PermissionResponse, AcpError> {
        Ok(v2::PermissionResponse {
            outcome: serde_json::Value::Null,
        })
    }
}

#[tokio::test]
async fn v1_initialization_returns_a_v2_bridge_handle() {
    let (caller, receiver) = InProcessTransport::pair();
    let caller = Peer::new(caller);
    let receiver = Peer::new(receiver);
    receiver.register::<InitializerHandle, _>(LegacyInitializer);
    receiver.register::<v1::AgentHandle, _>(LegacyAgent::default());

    let updates = Arc::new(Mutex::new(Vec::new()));
    let agent = initialize(
        caller,
        RecordingClient(updates.clone()),
        InitializeRequest {
            protocol_version: v2::PROTOCOL_VERSION,
            info: v2::Implementation {
                name: "test".into(),
                title: "Test".into(),
                version: None,
            },
            capabilities: v2::Capabilities::default(),
        },
    )
    .await
    .unwrap();

    let session = agent
        .new_session("/workspace".into(), Vec::new(), Vec::new())
        .await
        .unwrap();
    assert_eq!(session.session_id, "legacy-session");
    assert_eq!(
        agent
            .list_sessions(Some("/workspace".into()), None)
            .await
            .unwrap()
            .sessions[0]
            .title,
        Some("Remembered title".into())
    );
    agent
        .prompt(
            session.session_id.clone(),
            vec![serde_json::json!({"type": "text", "text": "hello"})],
        )
        .await
        .unwrap();
    assert_eq!(updates.lock().unwrap().len(), 2);
    assert_eq!(
        updates.lock().unwrap()[1].1,
        serde_json::json!({
            "sessionUpdate": "state_update",
            "state": "idle",
            "stopReason": "end_turn",
        })
    );
}

#[tokio::test]
async fn replay_from_start_loads_a_v1_session() {
    let (caller, receiver) = InProcessTransport::pair();
    let caller = Peer::new(caller);
    let receiver = Peer::new(receiver);
    receiver.register::<InitializerHandle, _>(LegacyInitializer);
    let legacy = LegacyAgent::default();
    let calls = legacy.calls.clone();
    receiver.register::<v1::AgentHandle, _>(legacy);

    let agent = initialize(
        caller,
        RecordingClient(Arc::new(Mutex::new(Vec::new()))),
        InitializeRequest {
            protocol_version: v2::PROTOCOL_VERSION,
            info: v2::Implementation {
                name: "test".into(),
                title: "Test".into(),
                version: None,
            },
            capabilities: v2::Capabilities::default(),
        },
    )
    .await
    .unwrap();

    agent
        .resume_session(
            "legacy-session".into(),
            "/workspace".into(),
            Vec::new(),
            Vec::new(),
            Some(v2::ReplayFrom::Start),
        )
        .await
        .unwrap();

    assert_eq!(*calls.lock().unwrap(), ["load"]);
}
