//! Provider-neutral RPC contract spoken between Atlas backends and frontends.
//!
//! The public model follows the app-server thread/turn/item lifecycle. Protocol
//! adapters live in separate crates; this crate owns aggregation and state.

use atlas_rpc::{CallError, InProcessTransport, Peer, RpcContext, Stream, interface};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use uuid::Uuid;

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }
        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

string_id!(BackendId);
string_id!(ThreadId);
string_id!(TurnId);
string_id!(ItemId);
string_id!(Cursor);
string_id!(QueuedSubmissionId);

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct ApiError {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ApiError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }

    pub fn with_code(message: impl Into<String>, code: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: Some(code.into()),
        }
    }

    fn from_call_error(error: CallError) -> Self {
        match error {
            CallError::Rpc(error) => error
                .data
                .and_then(|data| serde_json::from_value(data).ok())
                .unwrap_or_else(|| Self::new(error.message)),
            CallError::Closed => Self::new("RPC peer closed"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        uri: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
    },
    Audio {
        data: String,
        mime_type: String,
    },
    Resource {
        uri: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatus {
    Idle,
    Active,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Queued,
    InProgress,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreadItem {
    UserMessage {
        id: ItemId,
        content: Vec<ContentBlock>,
    },
    AgentMessage {
        id: ItemId,
        content: Vec<ContentBlock>,
    },
    Reasoning {
        id: ItemId,
        content: Vec<ContentBlock>,
    },
    Plan {
        id: ItemId,
        text: String,
    },
    CommandExecution {
        id: ItemId,
        command: String,
        cwd: String,
        status: ItemStatus,
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    FileChange {
        id: ItemId,
        changes: Vec<FileChange>,
        status: ItemStatus,
    },
    ToolCall {
        id: ItemId,
        title: String,
        status: ItemStatus,
        content: Vec<ContentBlock>,
    },
    Terminal {
        id: ItemId,
        title: String,
        status: ItemStatus,
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
}

impl ThreadItem {
    pub fn id(&self) -> &ItemId {
        match self {
            Self::UserMessage { id, .. }
            | Self::AgentMessage { id, .. }
            | Self::Reasoning { id, .. }
            | Self::Plan { id, .. }
            | Self::CommandExecution { id, .. }
            | Self::FileChange { id, .. }
            | Self::ToolCall { id, .. }
            | Self::Terminal { id, .. } => id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: TurnId,
    pub status: TurnStatus,
    #[serde(default)]
    pub items: Vec<ThreadItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    pub id: ThreadId,
    pub backend: BackendId,
    pub cwd: String,
    #[serde(default)]
    pub additional_directories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub status: ThreadStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    #[serde(flatten)]
    pub summary: ThreadSummary,
    #[serde(default)]
    pub turns: Vec<Turn>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedSubmission {
    pub id: QueuedSubmissionId,
    pub input: Vec<ContentBlock>,
    pub client_user_message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    pub cwd: String,
    #[serde(default)]
    pub additional_directories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartResponse {
    pub thread: ThreadSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadScope {
    Active,
    All,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListParams {
    pub scope: ThreadScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_term: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default)]
    pub subscribe: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListResponse {
    pub threads: Vec<ThreadSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<Cursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreadListEvent {
    Added { thread: ThreadSummary },
    Updated { thread: ThreadSummary },
    Removed { thread_id: ThreadId },
    Archived { thread_id: ThreadId },
    Deleted { thread_id: ThreadId },
    BackendUnavailable { backend: BackendId, message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadParams {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<Cursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadReadResponse {
    pub thread: Thread,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_cursor: Option<Cursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSubscribeParams {
    pub thread_id: ThreadId,
    #[serde(default = "default_tail")]
    pub tail_turns: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadUnsubscribeParams {
    pub thread_id: ThreadId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ThreadUnsubscribeStatus {
    NotLoaded,
    NotSubscribed,
    Unsubscribed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadUnsubscribeResponse {
    pub status: ThreadUnsubscribeStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadArchiveParams {
    pub thread_id: ThreadId,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ThreadArchiveResponse {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDeleteParams {
    pub thread_id: ThreadId,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ThreadDeleteResponse {}
fn default_tail() -> usize {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSnapshot {
    pub revision: u64,
    pub thread: Thread,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub older_cursor: Option<Cursor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeParams {
    pub thread_id: ThreadId,
    pub cwd: String,
    #[serde(default)]
    pub additional_directories: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: ThreadId,
    pub input: Vec<ContentBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_user_message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResponse {
    pub turn: Turn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQueueAddParams {
    pub thread_id: ThreadId,
    pub input: Vec<ContentBlock>,
    pub client_user_message_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQueueAddResponse {
    pub queued_submission: QueuedSubmission,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQueueListParams {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQueueListResponse {
    pub data: Vec<QueuedSubmission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQueueUpdateParams {
    pub thread_id: ThreadId,
    pub queued_submission_id: QueuedSubmissionId,
    pub input: Vec<ContentBlock>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQueueUpdateResponse {
    pub queued_submission: QueuedSubmission,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQueueDeleteParams {
    pub thread_id: ThreadId,
    pub queued_submission_id: QueuedSubmissionId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQueueDeleteResponse {
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQueueReorderParams {
    pub thread_id: ThreadId,
    pub queued_submission_ids: Vec<QueuedSubmissionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ThreadQueueReorderResponse {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQueueStartParams {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_submission_id: Option<QueuedSubmissionId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadQueueStartResponse {
    pub turn: Turn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ThreadEventKind {
    ThreadUpdated {
        thread: ThreadSummary,
    },
    TurnStarted {
        turn: Turn,
    },
    TurnCompleted {
        turn: Turn,
    },
    TurnFailed {
        turn: Turn,
    },
    ItemStarted {
        turn_id: TurnId,
        item: ThreadItem,
    },
    ItemUpdated {
        turn_id: TurnId,
        item: ThreadItem,
    },
    ItemCompleted {
        turn_id: TurnId,
        item: ThreadItem,
    },
    QueueChanged {
        thread_id: ThreadId,
    },
    UsageUpdated {
        input_tokens: u64,
        output_tokens: u64,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadEvent {
    pub revision: u64,
    #[serde(flatten)]
    pub event: ThreadEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalSubject {
    Command { command: String, cwd: String },
    FileChange { changes: Vec<FileChange> },
    ToolCall { title: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalOption {
    pub id: String,
    pub label: String,
    pub kind: ApprovalOptionKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub thread_id: ThreadId,
    pub turn_id: TurnId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ItemId>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub subject: ApprovalSubject,
    pub options: Vec<ApprovalOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ApprovalResponse {
    Selected { option_id: String },
    Cancelled,
}

#[interface]
pub trait Frontend {
    #[rpc(method = "item/requestApproval", payload)]
    async fn request_approval(
        &self,
        request: ApprovalRequest,
    ) -> Result<ApprovalResponse, ApiError>;
}

#[interface]
pub trait Backend {
    #[rpc(method = "thread/start", payload)]
    async fn thread_start(
        &self,
        request: ThreadStartParams,
    ) -> Result<ThreadStartResponse, ApiError>;
    #[rpc(method = "thread/list", reply_and_stream)]
    async fn thread_list(
        &self,
        request: ThreadListParams,
    ) -> Result<(ThreadListResponse, Stream<ThreadListEvent>), ApiError>;
    #[rpc(method = "thread/read", payload)]
    async fn thread_read(&self, request: ThreadReadParams) -> Result<ThreadReadResponse, ApiError>;
    #[rpc(method = "thread/subscribe", reply_and_stream)]
    async fn thread_subscribe(
        &self,
        #[rpc(context)] frontend: RpcContext<FrontendHandle>,
        request: ThreadSubscribeParams,
    ) -> Result<(ThreadSnapshot, Stream<ThreadEvent>), ApiError>;
    #[rpc(method = "thread/resume", payload)]
    async fn thread_resume(&self, request: ThreadResumeParams) -> Result<ThreadSummary, ApiError>;
    #[rpc(method = "thread/unsubscribe", payload)]
    async fn thread_unsubscribe(
        &self,
        #[rpc(context)] frontend: RpcContext<FrontendHandle>,
        request: ThreadUnsubscribeParams,
    ) -> Result<ThreadUnsubscribeResponse, ApiError>;
    #[rpc(method = "thread/archive", payload)]
    async fn thread_archive(
        &self,
        request: ThreadArchiveParams,
    ) -> Result<ThreadArchiveResponse, ApiError>;
    #[rpc(method = "thread/delete", payload)]
    async fn thread_delete(
        &self,
        request: ThreadDeleteParams,
    ) -> Result<ThreadDeleteResponse, ApiError>;
    #[rpc(method = "turn/start", payload, ordered)]
    async fn turn_start(
        &self,
        #[rpc(context)] frontend: RpcContext<FrontendHandle>,
        request: TurnStartParams,
    ) -> Result<TurnStartResponse, ApiError>;
    #[rpc(method = "turn/interrupt", payload)]
    async fn turn_interrupt(&self, request: TurnInterruptParams) -> Result<(), ApiError>;
    #[rpc(method = "thread/queue/add", payload, ordered)]
    async fn thread_queue_add(
        &self,
        #[rpc(context)] frontend: RpcContext<FrontendHandle>,
        request: ThreadQueueAddParams,
    ) -> Result<ThreadQueueAddResponse, ApiError>;
    #[rpc(method = "thread/queue/list", payload)]
    async fn thread_queue_list(
        &self,
        request: ThreadQueueListParams,
    ) -> Result<ThreadQueueListResponse, ApiError>;
    #[rpc(method = "thread/queue/update", payload)]
    async fn thread_queue_update(
        &self,
        request: ThreadQueueUpdateParams,
    ) -> Result<ThreadQueueUpdateResponse, ApiError>;
    #[rpc(method = "thread/queue/delete", payload)]
    async fn thread_queue_delete(
        &self,
        request: ThreadQueueDeleteParams,
    ) -> Result<ThreadQueueDeleteResponse, ApiError>;
    #[rpc(method = "thread/queue/reorder", payload)]
    async fn thread_queue_reorder(
        &self,
        request: ThreadQueueReorderParams,
    ) -> Result<ThreadQueueReorderResponse, ApiError>;
    #[rpc(method = "thread/queue/start", payload, ordered)]
    async fn thread_queue_start(
        &self,
        #[rpc(context)] frontend: RpcContext<FrontendHandle>,
        request: ThreadQueueStartParams,
    ) -> Result<ThreadQueueStartResponse, ApiError>;
}

#[derive(Clone)]
pub struct Multiplexer {
    state: Arc<Mutex<MultiplexerState>>,
    list_events: broadcast::Sender<ThreadListEvent>,
}

struct MultiplexerState {
    default_backend: BackendId,
    backends: BTreeMap<BackendId, BackendHandle>,
    outward: HashMap<ThreadId, (BackendId, ThreadId)>,
    inward: HashMap<(BackendId, ThreadId), ThreadId>,
    frontends: HashMap<(BackendId, ThreadId), FrontendHandle>,
    subscriptions: HashSet<(usize, BackendId, ThreadId)>,
    loaded: HashSet<(BackendId, ThreadId)>,
    idle_cleanup: HashMap<(BackendId, ThreadId), (u64, tokio::task::JoinHandle<()>)>,
    idle_generation: u64,
}

#[derive(Clone)]
struct MultiplexedFrontend {
    backend: BackendId,
    multiplexer: Multiplexer,
}

impl Frontend for MultiplexedFrontend {
    async fn request_approval(
        &self,
        mut request: ApprovalRequest,
    ) -> Result<ApprovalResponse, ApiError> {
        let (outward, frontend) = {
            let state = self.multiplexer.state.lock().unwrap();
            let key = (self.backend.clone(), request.thread_id.clone());
            let outward = state
                .inward
                .get(&key)
                .cloned()
                .ok_or_else(|| ApiError::new("approval references an unknown thread"))?;
            let frontend = state
                .frontends
                .get(&key)
                .cloned()
                .ok_or_else(|| ApiError::new("no frontend is attached to this thread"))?;
            (outward, frontend)
        };
        request.thread_id = outward;
        frontend
            .request_approval(request)
            .await
            .map_err(|error| ApiError::new(error.to_string()))
    }
}

impl Multiplexer {
    pub fn new(default_backend: BackendId) -> Self {
        let (list_events, _) = broadcast::channel(256);
        Self {
            state: Arc::new(Mutex::new(MultiplexerState {
                default_backend,
                backends: BTreeMap::new(),
                outward: HashMap::new(),
                inward: HashMap::new(),
                frontends: HashMap::new(),
                subscriptions: HashSet::new(),
                loaded: HashSet::new(),
                idle_cleanup: HashMap::new(),
                idle_generation: 0,
            })),
            list_events,
        }
    }

    pub fn add_backend(&self, id: BackendId, backend: BackendHandle) {
        self.state.lock().unwrap().backends.insert(id, backend);
    }

    /// Registers an in-process backend and wires its callbacks back through the
    /// frontend that initiated the corresponding outer turn.
    pub fn add_backend_service<T>(&self, id: BackendId, service: T)
    where
        T: Backend + Send + Sync + 'static,
    {
        let (caller_transport, receiver_transport) = InProcessTransport::pair();
        let caller = Peer::new(caller_transport);
        let receiver = Peer::new(receiver_transport);
        receiver.register::<BackendHandle, _>(service);
        caller.register::<FrontendHandle, _>(MultiplexedFrontend {
            backend: id.clone(),
            multiplexer: self.clone(),
        });
        self.add_backend(id, BackendHandle::new(caller));
    }

    pub fn remove_backend(&self, id: &BackendId, message: impl Into<String>) {
        self.state.lock().unwrap().backends.remove(id);
        let _ = self.list_events.send(ThreadListEvent::BackendUnavailable {
            backend: id.clone(),
            message: message.into(),
        });
    }

    pub fn register(&self, peer: &atlas_rpc::Peer) {
        peer.register::<BackendHandle, _>(self.clone());
        let connection_id = peer.connection_id();
        let peer = peer.clone();
        let multiplexer = self.clone();
        tokio::spawn(async move {
            peer.closed().await;
            multiplexer.connection_closed(connection_id);
        });
    }

    fn connection_closed(&self, connection_id: usize) {
        let cleanup = {
            let mut state = self.state.lock().unwrap();
            let affected: HashSet<_> = state
                .subscriptions
                .iter()
                .filter(|(id, _, _)| *id == connection_id)
                .map(|(_, backend, thread)| (backend.clone(), thread.clone()))
                .collect();
            state
                .subscriptions
                .retain(|(id, _, _)| *id != connection_id);
            affected
                .into_iter()
                .filter(|(backend, thread)| {
                    !state
                        .subscriptions
                        .iter()
                        .any(|(_, id, candidate)| id == backend && candidate == thread)
                })
                .filter_map(|(backend_id, thread_id)| {
                    state
                        .backends
                        .get(&backend_id)
                        .cloned()
                        .map(|backend| (backend_id, thread_id, backend))
                })
                .collect::<Vec<_>>()
        };
        for (backend_id, thread_id, backend) in cleanup {
            self.schedule_idle_cleanup(backend_id, thread_id, backend);
        }
    }

    fn backend(&self, id: &BackendId) -> Result<BackendHandle, ApiError> {
        self.state
            .lock()
            .unwrap()
            .backends
            .get(id)
            .cloned()
            .ok_or_else(|| ApiError::new(format!("backend {id} is unavailable")))
    }

    fn import_thread(&self, backend: &BackendId, mut thread: ThreadSummary) -> ThreadSummary {
        let key = (backend.clone(), thread.id.clone());
        let mut state = self.state.lock().unwrap();
        let outward = state.inward.get(&key).cloned().unwrap_or_else(|| {
            let value = ThreadId(
                Uuid::new_v5(
                    &Uuid::NAMESPACE_OID,
                    format!("{}\0{}", backend.0, thread.id.0).as_bytes(),
                )
                .to_string(),
            );
            state.inward.insert(key.clone(), value.clone());
            state.outward.insert(value.clone(), key);
            value
        });
        thread.id = outward;
        thread.backend = backend.clone();
        thread
    }

    fn mark_activity(&self, backend: &BackendId, thread_id: &ThreadId) {
        let key = (backend.clone(), thread_id.clone());
        let mut state = self.state.lock().unwrap();
        state.loaded.insert(key.clone());
        if let Some((_, cleanup)) = state.idle_cleanup.remove(&key) {
            cleanup.abort();
        }
    }

    fn schedule_idle_cleanup(
        &self,
        backend_id: BackendId,
        inner: ThreadId,
        backend: BackendHandle,
    ) {
        let key = (backend_id, inner.clone());
        let generation = {
            let mut state = self.state.lock().unwrap();
            state.idle_generation = state.idle_generation.wrapping_add(1);
            state.idle_generation
        };
        let multiplexer = self.clone();
        let cleanup_key = key.clone();
        let cleanup = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
            let _ = backend
                .thread_unsubscribe(ThreadUnsubscribeParams { thread_id: inner })
                .await;
            let mut state = multiplexer.state.lock().unwrap();
            if state
                .idle_cleanup
                .get(&cleanup_key)
                .is_some_and(|(current, _)| *current == generation)
            {
                state.idle_cleanup.remove(&cleanup_key);
                state.loaded.remove(&cleanup_key);
                state.frontends.remove(&cleanup_key);
            }
        });
        let mut state = self.state.lock().unwrap();
        if let Some((_, previous)) = state.idle_cleanup.insert(key, (generation, cleanup)) {
            previous.abort();
        }
    }

    fn clear_runtime_state(&self, backend: &BackendId, inner: &ThreadId) {
        let key = (backend.clone(), inner.clone());
        let mut state = self.state.lock().unwrap();
        state
            .subscriptions
            .retain(|(_, id, thread)| id != backend || thread != inner);
        state.loaded.remove(&key);
        state.frontends.remove(&key);
        if let Some((_, cleanup)) = state.idle_cleanup.remove(&key) {
            cleanup.abort();
        }
    }

    fn forget_thread(&self, backend: &BackendId, inner: &ThreadId) {
        self.clear_runtime_state(backend, inner);
        let mut state = self.state.lock().unwrap();
        if let Some(outward) = state.inward.remove(&(backend.clone(), inner.clone())) {
            state.outward.remove(&outward);
        }
    }

    fn resolve(
        &self,
        outward: &ThreadId,
    ) -> Result<(BackendId, ThreadId, BackendHandle), ApiError> {
        let state = self.state.lock().unwrap();
        let (backend_id, inner) = state
            .outward
            .get(outward)
            .cloned()
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        let backend = state
            .backends
            .get(&backend_id)
            .cloned()
            .ok_or_else(|| ApiError::new(format!("backend {backend_id} is unavailable")))?;
        Ok((backend_id, inner, backend))
    }

    fn rewrite_item_event(event: &mut ThreadEvent, outward: &ThreadId) {
        match &mut event.event {
            ThreadEventKind::ThreadUpdated { thread } => thread.id = outward.clone(),
            ThreadEventKind::QueueChanged { thread_id } => *thread_id = outward.clone(),
            _ => {}
        }
    }
}

impl Backend for Multiplexer {
    async fn thread_start(
        &self,
        mut request: ThreadStartParams,
    ) -> Result<ThreadStartResponse, ApiError> {
        let backend_id = request
            .backend
            .take()
            .unwrap_or_else(|| self.state.lock().unwrap().default_backend.clone());
        let response = self
            .backend(&backend_id)?
            .thread_start(request)
            .await
            .map_err(ApiError::from_call_error)?;
        self.mark_activity(&backend_id, &response.thread.id);
        Ok(ThreadStartResponse {
            thread: self.import_thread(&backend_id, response.thread),
        })
    }

    async fn thread_list(
        &self,
        request: ThreadListParams,
    ) -> Result<(ThreadListResponse, Stream<ThreadListEvent>), ApiError> {
        let backends = self.state.lock().unwrap().backends.clone();
        let mut threads = Vec::new();
        let mut streams: Vec<Pin<Box<dyn futures_util::Stream<Item = ThreadListEvent> + Send>>> =
            Vec::new();
        for (id, backend) in backends {
            let mut child_request = request.clone();
            child_request.cursor = None;
            child_request.limit = Some(100);
            let mut seen_cursors = std::collections::HashSet::new();
            loop {
                let (page, stream) = backend
                    .thread_list(child_request.clone())
                    .await
                    .map_err(ApiError::from_call_error)?;
                threads.extend(
                    page.threads
                        .into_iter()
                        .map(|thread| self.import_thread(&id, thread)),
                );
                if request.subscribe && child_request.cursor.is_none() {
                    let mux = self.clone();
                    let stream_backend = id.clone();
                    streams.push(Box::pin(stream.filter_map(move |result| {
                        let mux = mux.clone();
                        let id = stream_backend.clone();
                        async move {
                            result.ok().map(|event| match event {
                                ThreadListEvent::Added { thread } => ThreadListEvent::Added {
                                    thread: mux.import_thread(&id, thread),
                                },
                                ThreadListEvent::Updated { thread } => ThreadListEvent::Updated {
                                    thread: mux.import_thread(&id, thread),
                                },
                                ThreadListEvent::Removed { thread_id } => {
                                    let outward = mux
                                        .import_thread(
                                            &id,
                                            ThreadSummary {
                                                id: thread_id,
                                                backend: id.clone(),
                                                cwd: String::new(),
                                                additional_directories: Vec::new(),
                                                title: None,
                                                updated_at: None,
                                                status: ThreadStatus::Idle,
                                            },
                                        )
                                        .id;
                                    ThreadListEvent::Removed { thread_id: outward }
                                }
                                ThreadListEvent::Archived { thread_id } => {
                                    let outward = mux
                                        .import_thread(
                                            &id,
                                            ThreadSummary {
                                                id: thread_id,
                                                backend: id.clone(),
                                                cwd: String::new(),
                                                additional_directories: Vec::new(),
                                                title: None,
                                                updated_at: None,
                                                status: ThreadStatus::Idle,
                                            },
                                        )
                                        .id;
                                    ThreadListEvent::Archived { thread_id: outward }
                                }
                                ThreadListEvent::Deleted { thread_id } => {
                                    let inner = thread_id.clone();
                                    let outward = mux
                                        .import_thread(
                                            &id,
                                            ThreadSummary {
                                                id: thread_id,
                                                backend: id.clone(),
                                                cwd: String::new(),
                                                additional_directories: Vec::new(),
                                                title: None,
                                                updated_at: None,
                                                status: ThreadStatus::Idle,
                                            },
                                        )
                                        .id;
                                    mux.forget_thread(&id, &inner);
                                    ThreadListEvent::Deleted { thread_id: outward }
                                }
                                ThreadListEvent::BackendUnavailable { backend, message } => {
                                    ThreadListEvent::BackendUnavailable { backend, message }
                                }
                            })
                        }
                    })));
                }
                let Some(cursor) = page.next_cursor else {
                    break;
                };
                if !seen_cursors.insert(cursor.clone()) {
                    return Err(ApiError::new(format!(
                        "backend {id} returned a repeating cursor"
                    )));
                }
                child_request.cursor = Some(cursor);
                child_request.subscribe = false;
            }
        }
        threads.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        if let Some(search) = request.search_term.as_ref().map(|s| s.to_lowercase()) {
            threads.retain(|thread| {
                format!("{}\n{}", thread.title.as_deref().unwrap_or(""), thread.cwd)
                    .to_lowercase()
                    .contains(&search)
            });
        }
        let limit = request.limit.unwrap_or(100).clamp(1, 100);
        let offset = request
            .cursor
            .as_ref()
            .map(|cursor| {
                cursor
                    .0
                    .parse::<usize>()
                    .map_err(|_| ApiError::new("stale thread list cursor"))
            })
            .transpose()?
            .unwrap_or(0);
        if offset > threads.len() {
            return Err(ApiError::new("stale thread list cursor"));
        }
        let end = (offset + limit).min(threads.len());
        let next_cursor = (end < threads.len()).then(|| Cursor(end.to_string()));
        threads = threads[offset..end].to_vec();
        let local = tokio_stream::wrappers::BroadcastStream::new(self.list_events.subscribe())
            .filter_map(|event| async { event.ok() });
        streams.push(Box::pin(local));
        Ok((
            ThreadListResponse {
                threads,
                next_cursor,
            },
            Stream::new(futures_util::stream::select_all(streams)),
        ))
    }

    async fn thread_read(
        &self,
        mut request: ThreadReadParams,
    ) -> Result<ThreadReadResponse, ApiError> {
        let (backend_id, inner, backend) = self.resolve(&request.thread_id)?;
        let outward = request.thread_id.clone();
        request.thread_id = inner;
        let mut response = backend
            .thread_read(request)
            .await
            .map_err(ApiError::from_call_error)?;
        response.thread.summary = self.import_thread(&backend_id, response.thread.summary);
        response.thread.summary.id = outward;
        Ok(response)
    }

    async fn thread_subscribe(
        &self,
        frontend: RpcContext<FrontendHandle>,
        mut request: ThreadSubscribeParams,
    ) -> Result<(ThreadSnapshot, Stream<ThreadEvent>), ApiError> {
        let (backend_id, inner, backend) = self.resolve(&request.thread_id)?;
        let outward = request.thread_id.clone();
        self.mark_activity(&backend_id, &inner);
        self.state.lock().unwrap().subscriptions.insert((
            frontend.connection_id(),
            backend_id.clone(),
            inner.clone(),
        ));
        self.state.lock().unwrap().frontends.insert(
            (backend_id.clone(), inner.clone()),
            frontend.handle().clone(),
        );
        request.thread_id = inner;
        let (mut snapshot, stream) = backend
            .thread_subscribe(request)
            .await
            .map_err(ApiError::from_call_error)?;
        snapshot.thread.summary = self.import_thread(&backend_id, snapshot.thread.summary);
        snapshot.thread.summary.id = outward.clone();
        let events = stream.filter_map(move |event| {
            let outward = outward.clone();
            async move {
                event.ok().map(|mut event| {
                    Self::rewrite_item_event(&mut event, &outward);
                    event
                })
            }
        });
        Ok((snapshot, Stream::new(events)))
    }

    async fn thread_resume(
        &self,
        mut request: ThreadResumeParams,
    ) -> Result<ThreadSummary, ApiError> {
        let (backend_id, inner, backend) = self.resolve(&request.thread_id)?;
        request.thread_id = inner;
        let summary = backend
            .thread_resume(request)
            .await
            .map_err(ApiError::from_call_error)?;
        self.mark_activity(&backend_id, &summary.id);
        Ok(self.import_thread(&backend_id, summary))
    }

    async fn thread_unsubscribe(
        &self,
        frontend: RpcContext<FrontendHandle>,
        mut request: ThreadUnsubscribeParams,
    ) -> Result<ThreadUnsubscribeResponse, ApiError> {
        let Ok((backend_id, inner, backend)) = self.resolve(&request.thread_id) else {
            return Ok(ThreadUnsubscribeResponse {
                status: ThreadUnsubscribeStatus::NotLoaded,
            });
        };
        request.thread_id = inner.clone();
        let subscription = (frontend.connection_id(), backend_id.clone(), inner.clone());
        let should_schedule = {
            let mut state = self.state.lock().unwrap();
            if !state.loaded.contains(&(backend_id.clone(), inner.clone())) {
                return Ok(ThreadUnsubscribeResponse {
                    status: ThreadUnsubscribeStatus::NotLoaded,
                });
            }
            if !state.subscriptions.remove(&subscription) {
                return Ok(ThreadUnsubscribeResponse {
                    status: ThreadUnsubscribeStatus::NotSubscribed,
                });
            }
            !state
                .subscriptions
                .iter()
                .any(|(_, id, thread)| id == &backend_id && thread == &inner)
        };
        if should_schedule {
            self.schedule_idle_cleanup(backend_id, inner, backend);
        }
        Ok(ThreadUnsubscribeResponse {
            status: ThreadUnsubscribeStatus::Unsubscribed,
        })
    }

    async fn thread_archive(
        &self,
        mut request: ThreadArchiveParams,
    ) -> Result<ThreadArchiveResponse, ApiError> {
        let (backend_id, inner, backend) = self.resolve(&request.thread_id)?;
        request.thread_id = inner.clone();
        let response = backend
            .thread_archive(request)
            .await
            .map_err(ApiError::from_call_error)?;
        self.clear_runtime_state(&backend_id, &inner);
        Ok(response)
    }

    async fn thread_delete(
        &self,
        mut request: ThreadDeleteParams,
    ) -> Result<ThreadDeleteResponse, ApiError> {
        let (backend_id, inner, backend) = self.resolve(&request.thread_id)?;
        request.thread_id = inner.clone();
        let response = backend
            .thread_delete(request)
            .await
            .map_err(ApiError::from_call_error)?;
        self.forget_thread(&backend_id, &inner);
        Ok(response)
    }

    async fn turn_start(
        &self,
        frontend: RpcContext<FrontendHandle>,
        mut request: TurnStartParams,
    ) -> Result<TurnStartResponse, ApiError> {
        let (backend_id, inner, backend) = self.resolve(&request.thread_id)?;
        self.mark_activity(&backend_id, &inner);
        self.state
            .lock()
            .unwrap()
            .frontends
            .insert((backend_id, inner.clone()), frontend.handle().clone());
        request.thread_id = inner;
        backend
            .turn_start(request)
            .await
            .map_err(ApiError::from_call_error)
    }

    async fn turn_interrupt(&self, mut request: TurnInterruptParams) -> Result<(), ApiError> {
        let (_, inner, backend) = self.resolve(&request.thread_id)?;
        request.thread_id = inner;
        backend
            .turn_interrupt(request)
            .await
            .map_err(ApiError::from_call_error)
    }

    async fn thread_queue_add(
        &self,
        frontend: RpcContext<FrontendHandle>,
        mut request: ThreadQueueAddParams,
    ) -> Result<ThreadQueueAddResponse, ApiError> {
        let (backend_id, inner, backend) = self.resolve(&request.thread_id)?;
        self.mark_activity(&backend_id, &inner);
        self.state
            .lock()
            .unwrap()
            .frontends
            .insert((backend_id, inner.clone()), frontend.handle().clone());
        request.thread_id = inner;
        backend
            .thread_queue_add(request)
            .await
            .map_err(ApiError::from_call_error)
    }

    async fn thread_queue_list(
        &self,
        mut request: ThreadQueueListParams,
    ) -> Result<ThreadQueueListResponse, ApiError> {
        let (_, inner, backend) = self.resolve(&request.thread_id)?;
        request.thread_id = inner;
        backend
            .thread_queue_list(request)
            .await
            .map_err(ApiError::from_call_error)
    }

    async fn thread_queue_update(
        &self,
        mut request: ThreadQueueUpdateParams,
    ) -> Result<ThreadQueueUpdateResponse, ApiError> {
        let (_, inner, backend) = self.resolve(&request.thread_id)?;
        request.thread_id = inner;
        backend
            .thread_queue_update(request)
            .await
            .map_err(ApiError::from_call_error)
    }

    async fn thread_queue_delete(
        &self,
        mut request: ThreadQueueDeleteParams,
    ) -> Result<ThreadQueueDeleteResponse, ApiError> {
        let (_, inner, backend) = self.resolve(&request.thread_id)?;
        request.thread_id = inner;
        backend
            .thread_queue_delete(request)
            .await
            .map_err(ApiError::from_call_error)
    }

    async fn thread_queue_reorder(
        &self,
        mut request: ThreadQueueReorderParams,
    ) -> Result<ThreadQueueReorderResponse, ApiError> {
        let (_, inner, backend) = self.resolve(&request.thread_id)?;
        request.thread_id = inner;
        backend
            .thread_queue_reorder(request)
            .await
            .map_err(ApiError::from_call_error)
    }

    async fn thread_queue_start(
        &self,
        frontend: RpcContext<FrontendHandle>,
        mut request: ThreadQueueStartParams,
    ) -> Result<ThreadQueueStartResponse, ApiError> {
        let (backend_id, inner, backend) = self.resolve(&request.thread_id)?;
        self.mark_activity(&backend_id, &inner);
        self.state
            .lock()
            .unwrap()
            .frontends
            .insert((backend_id, inner.clone()), frontend.handle().clone());
        request.thread_id = inner;
        backend
            .thread_queue_start(request)
            .await
            .map_err(ApiError::from_call_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(id: &str) -> ThreadSummary {
        ThreadSummary {
            id: ThreadId(id.into()),
            backend: BackendId("ignored".into()),
            cwd: "/workspace".into(),
            additional_directories: Vec::new(),
            title: None,
            updated_at: None,
            status: ThreadStatus::Idle,
        }
    }

    #[test]
    fn multiplexed_thread_ids_are_stable_and_backend_scoped() {
        let multiplexer = Multiplexer::new(BackendId("one".into()));
        let one = BackendId("one".into());
        let two = BackendId("two".into());
        let first = multiplexer.import_thread(&one, thread("same"));
        let repeated = multiplexer.import_thread(&one, thread("same"));
        let other_backend = multiplexer.import_thread(&two, thread("same"));

        assert_eq!(first.id, repeated.id);
        assert_ne!(first.id, other_backend.id);
        assert_eq!(first.backend, one);
        assert_eq!(other_backend.backend, two);
    }

    #[test]
    fn approval_subjects_have_a_closed_wire_shape() {
        let value = serde_json::to_value(ApprovalSubject::Command {
            command: "cargo test".into(),
            cwd: "/workspace".into(),
        })
        .unwrap();
        assert_eq!(value["type"], "command");
        assert_eq!(value["command"], "cargo test");
        assert!(value.get("unknownExtension").is_none());
    }

    #[test]
    fn queue_payloads_use_codex_wire_names() {
        let value = serde_json::to_value(ThreadQueueAddParams {
            thread_id: ThreadId("thread".into()),
            input: vec![ContentBlock::Text {
                text: "follow up".into(),
            }],
            client_user_message_id: "message".into(),
        })
        .unwrap();
        assert_eq!(value["threadId"], "thread");
        assert_eq!(value["clientUserMessageId"], "message");
        assert_eq!(value["input"][0]["type"], "text");
    }

    #[test]
    fn lifecycle_payloads_use_codex_wire_shapes() {
        let params = serde_json::to_value(ThreadUnsubscribeParams {
            thread_id: ThreadId("thread".into()),
        })
        .unwrap();
        let response = serde_json::to_value(ThreadUnsubscribeResponse {
            status: ThreadUnsubscribeStatus::NotSubscribed,
        })
        .unwrap();
        assert_eq!(params, serde_json::json!({"threadId": "thread"}));
        assert_eq!(response, serde_json::json!({"status": "notSubscribed"}));
        assert_eq!(
            serde_json::to_value(ThreadScope::Archived).unwrap(),
            serde_json::json!("archived")
        );
    }

    #[test]
    fn multiplexer_preserves_application_error_codes() {
        let source = ApiError::with_code("busy", "thread_busy");
        let restored =
            ApiError::from_call_error(CallError::Rpc(atlas_rpc::RpcError::application(source)));
        assert_eq!(restored.message, "busy");
        assert_eq!(restored.code.as_deref(), Some("thread_busy"));
    }
}
