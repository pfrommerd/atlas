//! ACP v1/v2 adapters for the provider-neutral Atlas API.

use crate::{AcpError, InitializeRequest, initialize_with_response, v2};
use atlas_agent::{
    ApiError, ApprovalOption, ApprovalOptionKind, ApprovalRequest, ApprovalResponse,
    ApprovalSubject, Backend, BackendId, ContentBlock, Cursor, FrontendHandle, ItemId, ItemStatus,
    QueuedSubmission, QueuedSubmissionId, Thread, ThreadArchiveParams, ThreadArchiveResponse,
    ThreadDeleteParams, ThreadDeleteResponse, ThreadEvent, ThreadEventKind, ThreadId, ThreadItem,
    ThreadListEvent, ThreadListParams, ThreadListResponse, ThreadQueueAddParams,
    ThreadQueueAddResponse, ThreadQueueDeleteParams, ThreadQueueDeleteResponse,
    ThreadQueueListParams, ThreadQueueListResponse, ThreadQueueReorderParams,
    ThreadQueueReorderResponse, ThreadQueueStartParams, ThreadQueueStartResponse,
    ThreadQueueUpdateParams, ThreadQueueUpdateResponse, ThreadReadParams, ThreadReadResponse,
    ThreadResumeParams, ThreadScope, ThreadSnapshot, ThreadStartParams, ThreadStartResponse,
    ThreadStatus, ThreadSubscribeParams, ThreadSummary, ThreadUnsubscribeParams,
    ThreadUnsubscribeResponse, ThreadUnsubscribeStatus, Turn, TurnId, TurnInterruptParams,
    TurnStartParams, TurnStartResponse, TurnStatus,
};
use atlas_rpc::{Peer, RpcContext, Stream};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use uuid::Uuid;

pub struct SpawnedAcp {
    pub bridge: AcpBridge,
    pub child: tokio::process::Child,
}

pub async fn spawn(
    backend: BackendId,
    command: &str,
    args: &[String],
) -> Result<SpawnedAcp, Box<dyn std::error::Error + Send + Sync>> {
    spawn_with_queue_store(backend, command, args, AcpQueueStore::default()).await
}

pub async fn spawn_with_queue_store(
    backend: BackendId,
    command: &str,
    args: &[String],
    queue_store: AcpQueueStore,
) -> Result<SpawnedAcp, Box<dyn std::error::Error + Send + Sync>> {
    let mut child = tokio::process::Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdin = child.stdin.take().expect("piped ACP stdin");
    let stdout = child.stdout.take().expect("piped ACP stdout");
    let peer = Peer::new(atlas_rpc::JsonTransport(StdioTransport::new(stdin, stdout)));
    match AcpBridge::connect_with_queue_store(peer, backend, queue_store).await {
        Ok(bridge) => Ok(SpawnedAcp { bridge, child }),
        Err(error) => {
            let _ = child.kill().await;
            Err(Box::new(error))
        }
    }
}

struct StdioTransport {
    incoming: tokio_stream::wrappers::UnboundedReceiverStream<Result<String, io::Error>>,
    outgoing: tokio::sync::mpsc::UnboundedSender<String>,
}

impl StdioTransport {
    fn new(mut stdin: tokio::process::ChildStdin, stdout: tokio::process::ChildStdout) -> Self {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (incoming_tx, incoming_rx) = tokio::sync::mpsc::unbounded_channel();
        let (outgoing_tx, mut outgoing_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = incoming_tx.send(Ok(line));
            }
        });
        tokio::spawn(async move {
            while let Some(line) = outgoing_rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err()
                    || stdin.write_all(b"\n").await.is_err()
                {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });
        Self {
            incoming: tokio_stream::wrappers::UnboundedReceiverStream::new(incoming_rx),
            outgoing: outgoing_tx,
        }
    }
}

impl futures_util::Stream for StdioTransport {
    type Item = Result<String, io::Error>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        std::pin::Pin::new(&mut self.incoming).poll_next(cx)
    }
}

impl futures_util::Sink<String> for StdioTransport {
    type Error = io::Error;
    fn poll_ready(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn start_send(self: std::pin::Pin<&mut Self>, item: String) -> Result<(), Self::Error> {
        self.outgoing
            .send(item)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "ACP stdin closed"))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }
}

#[derive(Clone)]
pub struct AcpBridge {
    agent: v2::AgentHandle,
    backend: BackendId,
    state: BridgeState,
    supports_close: bool,
}

#[derive(Clone)]
struct BridgeState {
    sessions: Arc<Mutex<HashMap<String, SessionState>>>,
    list_events: broadcast::Sender<ThreadListEvent>,
    queues: AcpQueueStore,
}

struct SessionState {
    summary: ThreadSummary,
    turns: Vec<Turn>,
    active_turn: Option<TurnId>,
    frontend: Option<FrontendHandle>,
    revision: u64,
    events: broadcast::Sender<ThreadEvent>,
}

#[derive(Clone, Default)]
pub struct AcpQueueStore {
    queues: Arc<Mutex<HashMap<String, StoredQueue>>>,
}

#[derive(Default)]
struct StoredQueue {
    submissions: VecDeque<QueuedSubmission>,
    paused: bool,
    revision: u64,
}

impl AcpQueueStore {
    pub fn pause_all(&self) {
        for queue in self.queues.lock().unwrap().values_mut() {
            if !queue.submissions.is_empty() {
                queue.paused = true;
                queue.revision = queue.revision.wrapping_add(1);
            }
        }
    }
}

impl BridgeState {
    fn new(queues: AcpQueueStore) -> Self {
        let (list_events, _) = broadcast::channel(256);
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            list_events,
            queues,
        }
    }

    fn upsert_summary(&self, summary: ThreadSummary) {
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(session) = sessions.get_mut(&summary.id.0) {
            session.summary = summary.clone();
            let _ = self
                .list_events
                .send(ThreadListEvent::Updated { thread: summary });
        } else {
            let (events, _) = broadcast::channel(512);
            sessions.insert(
                summary.id.0.clone(),
                SessionState {
                    summary: summary.clone(),
                    turns: Vec::new(),
                    active_turn: None,
                    frontend: None,
                    revision: 0,
                    events,
                },
            );
            let _ = self
                .list_events
                .send(ThreadListEvent::Added { thread: summary });
        }
    }

    fn emit(session: &mut SessionState, event: ThreadEventKind) {
        session.revision = session.revision.wrapping_add(1);
        let _ = session.events.send(ThreadEvent {
            revision: session.revision,
            event,
        });
    }

    fn apply_update(&self, session_id: &str, update: Value) {
        let Some(kind) = update.get("sessionUpdate").and_then(Value::as_str) else {
            return;
        };
        let mut sessions = self.sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(session_id) else {
            return;
        };
        match kind {
            "session_info_update" => {
                if let Some(title) = update.get("title").and_then(Value::as_str) {
                    session.summary.title = Some(title.to_owned());
                }
                if let Some(updated) = update.get("updatedAt").and_then(Value::as_str) {
                    session.summary.updated_at = Some(updated.to_owned());
                }
                Self::emit(
                    session,
                    ThreadEventKind::ThreadUpdated {
                        thread: session.summary.clone(),
                    },
                );
            }
            "user_message"
            | "user_message_chunk"
            | "agent_message"
            | "agent_message_chunk"
            | "agent_thought"
            | "agent_thought_chunk" => {
                let Some(turn_id) = session
                    .active_turn
                    .clone()
                    .or_else(|| session.turns.last().map(|turn| turn.id.clone()))
                else {
                    return;
                };
                let role = if kind.starts_with("user_") {
                    "user"
                } else if kind.starts_with("agent_thought") {
                    "reasoning"
                } else {
                    "agent"
                };
                let message_id = update
                    .get("messageId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("legacy-{role}-{}", session.revision + 1));
                let item_id = ItemId(format!("{role}:{message_id}"));
                let mut content = Vec::new();
                if let Some(values) = update.get("content").and_then(Value::as_array) {
                    content.extend(values.iter().filter_map(content_block));
                } else if let Some(value) = update.get("content").and_then(content_block) {
                    content.push(value);
                }
                let Some(turn) = session.turns.iter_mut().find(|turn| turn.id == turn_id) else {
                    return;
                };
                let position = turn.items.iter().position(|item| item.id() == &item_id);
                let mut item = match role {
                    "user" => ThreadItem::UserMessage {
                        id: item_id.clone(),
                        content,
                    },
                    "reasoning" => ThreadItem::Reasoning {
                        id: item_id.clone(),
                        content,
                    },
                    _ => ThreadItem::AgentMessage {
                        id: item_id.clone(),
                        content,
                    },
                };
                let started = position.is_none();
                if let Some(index) = position {
                    append_content(&mut turn.items[index], &mut item, kind.ends_with("_chunk"));
                    item = turn.items[index].clone();
                } else {
                    turn.items.push(item.clone());
                }
                Self::emit(
                    session,
                    if started {
                        ThreadEventKind::ItemStarted { turn_id, item }
                    } else {
                        ThreadEventKind::ItemUpdated { turn_id, item }
                    },
                );
            }
            "tool_call_update" | "tool_call_content_chunk" => {
                let Some(turn_id) = session.active_turn.clone() else {
                    return;
                };
                let id = ItemId(
                    update
                        .get("toolCallId")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_owned(),
                );
                let title = update
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Tool")
                    .to_owned();
                let status = item_status(update.get("status").and_then(Value::as_str));
                let content = update
                    .get("content")
                    .and_then(Value::as_array)
                    .map(|values| values.iter().filter_map(content_block).collect())
                    .unwrap_or_default();
                let item = ThreadItem::ToolCall {
                    id: id.clone(),
                    title,
                    status,
                    content,
                };
                let Some(turn) = session.turns.iter_mut().find(|turn| turn.id == turn_id) else {
                    return;
                };
                let started =
                    if let Some(index) = turn.items.iter().position(|entry| entry.id() == &id) {
                        turn.items[index] = item.clone();
                        false
                    } else {
                        turn.items.push(item.clone());
                        true
                    };
                Self::emit(
                    session,
                    if started {
                        ThreadEventKind::ItemStarted { turn_id, item }
                    } else {
                        ThreadEventKind::ItemUpdated { turn_id, item }
                    },
                );
            }
            "plan_update" => {
                let Some(turn_id) = session.active_turn.clone() else {
                    return;
                };
                let id = ItemId(
                    update
                        .get("planId")
                        .and_then(Value::as_str)
                        .unwrap_or("plan")
                        .to_owned(),
                );
                let text = update
                    .get("entries")
                    .or_else(|| update.get("content"))
                    .map(Value::to_string)
                    .unwrap_or_default();
                let item = ThreadItem::Plan {
                    id: id.clone(),
                    text,
                };
                let Some(turn) = session.turns.iter_mut().find(|turn| turn.id == turn_id) else {
                    return;
                };
                let started =
                    if let Some(index) = turn.items.iter().position(|entry| entry.id() == &id) {
                        turn.items[index] = item.clone();
                        false
                    } else {
                        turn.items.push(item.clone());
                        true
                    };
                Self::emit(
                    session,
                    if started {
                        ThreadEventKind::ItemStarted { turn_id, item }
                    } else {
                        ThreadEventKind::ItemUpdated { turn_id, item }
                    },
                );
            }
            "state_update" if update.get("state").and_then(Value::as_str) == Some("idle") => {
                let Some(turn_id) = session.active_turn.take() else {
                    return;
                };
                if let Some(turn) = session.turns.iter_mut().find(|turn| turn.id == turn_id) {
                    turn.status =
                        if update.get("stopReason").and_then(Value::as_str) == Some("cancelled") {
                            TurnStatus::Interrupted
                        } else {
                            TurnStatus::Completed
                        };
                    turn.stop_reason = update
                        .get("stopReason")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let completed = turn.clone();
                    session.summary.status = ThreadStatus::Idle;
                    Self::emit(session, ThreadEventKind::TurnCompleted { turn: completed });
                }
            }
            _ => {}
        }
    }
}

fn prompt_content(input: Vec<ContentBlock>) -> Vec<Value> {
    input
        .into_iter()
        .map(|block| match block {
            ContentBlock::Text { text } => json!({"type":"text","text":text}),
            ContentBlock::Image { uri, mime_type } => {
                json!({"type":"image","uri":uri,"mimeType":mime_type})
            }
            ContentBlock::Audio { data, mime_type } => {
                json!({"type":"audio","data":data,"mimeType":mime_type})
            }
            ContentBlock::Resource { uri, name } => {
                json!({"type":"resource","uri":uri,"name":name})
            }
        })
        .collect()
}

fn turn_from_submission(submission: &QueuedSubmission) -> Turn {
    Turn {
        id: TurnId(Uuid::new_v4().to_string()),
        status: TurnStatus::InProgress,
        items: vec![ThreadItem::UserMessage {
            id: ItemId(submission.client_user_message_id.clone()),
            content: submission.input.clone(),
        }],
        stop_reason: None,
        error: None,
    }
}

fn spawn_prompt(
    agent: v2::AgentHandle,
    state: BridgeState,
    session_id: String,
    turn_id: TurnId,
    prompt: Vec<Value>,
) {
    tokio::spawn(async move {
        let result = agent.prompt(session_id.clone(), prompt).await;
        let backend_closed = matches!(result, Err(atlas_rpc::CallError::Closed));
        let next = {
            let mut sessions = state.sessions.lock().unwrap();
            let Some(session) = sessions.get_mut(&session_id) else {
                return;
            };
            if session.active_turn.as_ref() == Some(&turn_id) {
                let id = session.active_turn.take().expect("active turn was checked");
                if let Some(turn) = session.turns.iter_mut().find(|turn| turn.id == id) {
                    match result {
                        Ok(()) => turn.status = TurnStatus::Completed,
                        Err(ref error) => {
                            turn.status = TurnStatus::Failed;
                            turn.error = Some(error.to_string());
                        }
                    }
                    let completed = turn.clone();
                    BridgeState::emit(
                        session,
                        if result.is_ok() {
                            ThreadEventKind::TurnCompleted { turn: completed }
                        } else {
                            ThreadEventKind::TurnFailed { turn: completed }
                        },
                    );
                }
            }
            if session.active_turn.is_some() {
                return;
            }
            session.summary.status = ThreadStatus::Idle;
            let submission = {
                let mut queues = state.queues.queues.lock().unwrap();
                let queue = queues.entry(session_id.clone()).or_default();
                if backend_closed && !queue.submissions.is_empty() {
                    queue.paused = true;
                    queue.revision = queue.revision.wrapping_add(1);
                    None
                } else if queue.paused {
                    None
                } else {
                    let submission = queue.submissions.pop_front();
                    if submission.is_some() {
                        queue.revision = queue.revision.wrapping_add(1);
                    }
                    submission
                }
            };
            submission.map(|submission| {
                let turn = turn_from_submission(&submission);
                session.active_turn = Some(turn.id.clone());
                session.summary.status = ThreadStatus::Active;
                session.turns.push(turn.clone());
                BridgeState::emit(
                    session,
                    ThreadEventKind::QueueChanged {
                        thread_id: session.summary.id.clone(),
                    },
                );
                BridgeState::emit(session, ThreadEventKind::TurnStarted { turn: turn.clone() });
                (turn.id, submission.input)
            })
        };
        if let Some((turn_id, input)) = next {
            spawn_prompt(agent, state, session_id, turn_id, prompt_content(input));
        }
    });
}

fn content_block(value: &Value) -> Option<ContentBlock> {
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        return Some(ContentBlock::Text {
            text: text.to_owned(),
        });
    }
    value.as_str().map(|text| ContentBlock::Text {
        text: text.to_owned(),
    })
}

fn append_content(existing: &mut ThreadItem, replacement: &mut ThreadItem, append: bool) {
    let (old, new) = match (existing, replacement) {
        (
            ThreadItem::UserMessage { content: old, .. },
            ThreadItem::UserMessage { content: new, .. },
        )
        | (
            ThreadItem::AgentMessage { content: old, .. },
            ThreadItem::AgentMessage { content: new, .. },
        )
        | (
            ThreadItem::Reasoning { content: old, .. },
            ThreadItem::Reasoning { content: new, .. },
        ) => (old, new),
        _ => return,
    };
    if append {
        old.append(new);
    } else {
        *old = std::mem::take(new);
    }
}

fn item_status(status: Option<&str>) -> ItemStatus {
    match status {
        Some("in_progress") => ItemStatus::InProgress,
        Some("completed") => ItemStatus::Completed,
        Some("failed") => ItemStatus::Failed,
        _ => ItemStatus::Pending,
    }
}

#[derive(Clone)]
struct AcpClient {
    state: BridgeState,
}

impl v2::Client for AcpClient {
    async fn session_update(&self, session_id: String, update: Value) -> Result<(), AcpError> {
        self.state.apply_update(&session_id, update);
        Ok(())
    }

    async fn request_permission(
        &self,
        session_id: String,
        title: String,
        subject: Option<String>,
        options: Vec<Value>,
    ) -> Result<v2::PermissionResponse, AcpError> {
        let (frontend, turn_id, thread_id) = {
            let sessions = self.state.sessions.lock().unwrap();
            let session = sessions
                .get(&session_id)
                .ok_or_else(|| AcpError::new("unknown session"))?;
            (
                session.frontend.clone(),
                session.active_turn.clone(),
                session.summary.id.clone(),
            )
        };
        let Some(frontend) = frontend else {
            return Err(AcpError::new(
                "no frontend can answer this permission request",
            ));
        };
        let request = ApprovalRequest {
            thread_id,
            turn_id: turn_id.ok_or_else(|| AcpError::new("permission requested outside a turn"))?,
            item_id: None,
            title,
            description: subject.clone(),
            subject: ApprovalSubject::ToolCall {
                title: subject.unwrap_or_else(|| "Tool call".into()),
            },
            options: options
                .iter()
                .enumerate()
                .map(|(index, option)| ApprovalOption {
                    id: option
                        .get("optionId")
                        .or_else(|| option.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .unwrap_or_else(|| index.to_string()),
                    label: option
                        .get("name")
                        .or_else(|| option.get("label"))
                        .and_then(Value::as_str)
                        .unwrap_or("Option")
                        .to_owned(),
                    kind: match option.get("kind").and_then(Value::as_str) {
                        Some("allow_always") => ApprovalOptionKind::AllowAlways,
                        Some("reject_always") => ApprovalOptionKind::RejectAlways,
                        Some("reject_once") => ApprovalOptionKind::RejectOnce,
                        _ => ApprovalOptionKind::AllowOnce,
                    },
                })
                .collect(),
        };
        let response = frontend
            .request_approval(request)
            .await
            .map_err(|error| AcpError::new(error.to_string()))?;
        Ok(v2::PermissionResponse {
            outcome: match response {
                ApprovalResponse::Selected { option_id } => {
                    json!({"outcome":"selected","optionId":option_id})
                }
                ApprovalResponse::Cancelled => json!({"outcome":"cancelled"}),
            },
        })
    }
}

impl AcpBridge {
    pub async fn connect(peer: Peer, backend: BackendId) -> Result<Self, atlas_rpc::CallError> {
        Self::connect_with_queue_store(peer, backend, AcpQueueStore::default()).await
    }

    pub async fn connect_with_queue_store(
        peer: Peer,
        backend: BackendId,
        queue_store: AcpQueueStore,
    ) -> Result<Self, atlas_rpc::CallError> {
        let state = BridgeState::new(queue_store);
        let (agent, response) = initialize_with_response(
            peer,
            AcpClient {
                state: state.clone(),
            },
            InitializeRequest {
                protocol_version: v2::PROTOCOL_VERSION,
                info: v2::Implementation {
                    name: "atlas".into(),
                    title: "Atlas".into(),
                    version: Some(env!("CARGO_PKG_VERSION").into()),
                },
                capabilities: v2::Capabilities::default(),
            },
        )
        .await?;
        let supports_close = response
            .capabilities
            .session
            .as_ref()
            .and_then(|session| session.get("close"))
            .is_some();
        Ok(Self {
            agent,
            backend,
            state,
            supports_close,
        })
    }

    fn summary(&self, info: v2::SessionInfo) -> ThreadSummary {
        ThreadSummary {
            id: ThreadId(info.session_id),
            backend: self.backend.clone(),
            cwd: info.cwd,
            additional_directories: info.additional_directories,
            title: info.title,
            updated_at: info.updated_at,
            status: ThreadStatus::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_rpc::InProcessTransport;

    #[tokio::test]
    async fn queue_mutations_invalidate_list_cursors() {
        let state = BridgeState::new(AcpQueueStore::default());
        let thread_id = ThreadId("thread".into());
        state.upsert_summary(ThreadSummary {
            id: thread_id.clone(),
            backend: BackendId("test".into()),
            cwd: "/workspace".into(),
            additional_directories: Vec::new(),
            title: None,
            updated_at: None,
            status: ThreadStatus::Active,
        });
        state.queues.queues.lock().unwrap().insert(
            thread_id.0.clone(),
            StoredQueue {
                submissions: ["one", "two"]
                    .into_iter()
                    .map(|id| QueuedSubmission {
                        id: QueuedSubmissionId(id.into()),
                        input: vec![ContentBlock::Text { text: id.into() }],
                        client_user_message_id: format!("message-{id}"),
                    })
                    .collect(),
                paused: false,
                revision: 7,
            },
        );
        let (transport, _other) = InProcessTransport::pair();
        let bridge = AcpBridge {
            agent: v2::AgentHandle::new(Peer::new(transport)),
            backend: BackendId("test".into()),
            state,
            supports_close: false,
        };

        let archive_error = bridge
            .thread_archive(ThreadArchiveParams {
                thread_id: thread_id.clone(),
            })
            .await
            .unwrap_err();
        let delete_error = bridge
            .thread_delete(ThreadDeleteParams {
                thread_id: thread_id.clone(),
            })
            .await
            .unwrap_err();
        assert_eq!(archive_error.code.as_deref(), Some("unsupported_operation"));
        assert_eq!(delete_error.code.as_deref(), Some("unsupported_operation"));

        let first_page = bridge
            .thread_queue_list(ThreadQueueListParams {
                thread_id: thread_id.clone(),
                cursor: None,
                limit: Some(1),
            })
            .await
            .unwrap();
        assert_eq!(first_page.data[0].id, QueuedSubmissionId("one".into()));
        let cursor = first_page.next_cursor.unwrap();

        bridge
            .thread_queue_update(ThreadQueueUpdateParams {
                thread_id: thread_id.clone(),
                queued_submission_id: QueuedSubmissionId("one".into()),
                input: vec![ContentBlock::Text {
                    text: "updated".into(),
                }],
            })
            .await
            .unwrap();
        let error = bridge
            .thread_queue_list(ThreadQueueListParams {
                thread_id,
                cursor: Some(cursor),
                limit: Some(1),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code.as_deref(), Some("stale_queue_cursor"));
    }

    #[tokio::test]
    async fn reorder_requires_an_exact_permutation() {
        let state = BridgeState::new(AcpQueueStore::default());
        let thread_id = ThreadId("thread".into());
        state.upsert_summary(ThreadSummary {
            id: thread_id.clone(),
            backend: BackendId("test".into()),
            cwd: "/workspace".into(),
            additional_directories: Vec::new(),
            title: None,
            updated_at: None,
            status: ThreadStatus::Active,
        });
        state.queues.queues.lock().unwrap().insert(
            thread_id.0.clone(),
            StoredQueue {
                submissions: ["one", "two"]
                    .into_iter()
                    .map(|id| QueuedSubmission {
                        id: QueuedSubmissionId(id.into()),
                        input: Vec::new(),
                        client_user_message_id: id.into(),
                    })
                    .collect(),
                paused: false,
                revision: 0,
            },
        );
        let (transport, _other) = InProcessTransport::pair();
        let bridge = AcpBridge {
            agent: v2::AgentHandle::new(Peer::new(transport)),
            backend: BackendId("test".into()),
            state,
            supports_close: false,
        };

        let error = bridge
            .thread_queue_reorder(ThreadQueueReorderParams {
                thread_id,
                queued_submission_ids: vec![QueuedSubmissionId("one".into())],
            })
            .await
            .unwrap_err();
        assert_eq!(error.code.as_deref(), Some("invalid_queue_order"));
    }
}

impl Backend for AcpBridge {
    async fn thread_start(
        &self,
        request: ThreadStartParams,
    ) -> Result<ThreadStartResponse, ApiError> {
        let response = self
            .agent
            .new_session(
                request.cwd.clone(),
                request.additional_directories.clone(),
                Vec::new(),
            )
            .await
            .map_err(|e| ApiError::new(e.to_string()))?;
        let summary = ThreadSummary {
            id: ThreadId(response.session_id),
            backend: self.backend.clone(),
            cwd: request.cwd,
            additional_directories: request.additional_directories,
            title: None,
            updated_at: None,
            status: ThreadStatus::Idle,
        };
        self.state.upsert_summary(summary.clone());
        Ok(ThreadStartResponse { thread: summary })
    }

    async fn thread_list(
        &self,
        request: ThreadListParams,
    ) -> Result<(ThreadListResponse, Stream<ThreadListEvent>), ApiError> {
        if request.scope == ThreadScope::Archived {
            return Ok((
                ThreadListResponse {
                    threads: Vec::new(),
                    next_cursor: None,
                },
                Stream::new(futures_util::stream::empty()),
            ));
        }
        let response = self
            .agent
            .list_sessions(None, request.cursor.map(|cursor| cursor.0))
            .await
            .map_err(|e| ApiError::new(e.to_string()))?;
        let mut threads: Vec<_> = response
            .sessions
            .into_iter()
            .map(|info| self.summary(info))
            .collect();
        for thread in &threads {
            self.state.upsert_summary(thread.clone());
        }
        if request.scope == ThreadScope::Active {
            threads.retain(|thread| thread.status == ThreadStatus::Active);
        }
        let page = ThreadListResponse {
            threads,
            next_cursor: response.next_cursor.map(Cursor),
        };
        if !request.subscribe {
            return Ok((page, Stream::new(futures_util::stream::empty())));
        }
        let events =
            tokio_stream::wrappers::BroadcastStream::new(self.state.list_events.subscribe())
                .filter_map(|event| async { event.ok() });
        Ok((page, Stream::new(events)))
    }

    async fn thread_read(&self, request: ThreadReadParams) -> Result<ThreadReadResponse, ApiError> {
        let sessions = self.state.sessions.lock().unwrap();
        let session = sessions
            .get(&request.thread_id.0)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        let limit = request.limit.unwrap_or(20).clamp(1, 100);
        let end = request
            .before
            .as_ref()
            .and_then(|cursor| cursor.0.parse::<usize>().ok())
            .unwrap_or(session.turns.len())
            .min(session.turns.len());
        let start = end.saturating_sub(limit);
        Ok(ThreadReadResponse {
            thread: Thread {
                summary: session.summary.clone(),
                turns: session.turns[start..end].to_vec(),
            },
            older_cursor: (start > 0).then(|| Cursor(start.to_string())),
        })
    }

    async fn thread_subscribe(
        &self,
        frontend: RpcContext<FrontendHandle>,
        request: ThreadSubscribeParams,
    ) -> Result<(ThreadSnapshot, Stream<ThreadEvent>), ApiError> {
        let mut sessions = self.state.sessions.lock().unwrap();
        let session = sessions
            .get_mut(&request.thread_id.0)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        session.frontend = Some(frontend.handle().clone());
        let start = session
            .turns
            .len()
            .saturating_sub(request.tail_turns.clamp(1, 100));
        let snapshot = ThreadSnapshot {
            revision: session.revision,
            thread: Thread {
                summary: session.summary.clone(),
                turns: session.turns[start..].to_vec(),
            },
            older_cursor: (start > 0).then(|| Cursor(start.to_string())),
        };
        let events = tokio_stream::wrappers::BroadcastStream::new(session.events.subscribe())
            .filter_map(|event| async { event.ok() });
        Ok((snapshot, Stream::new(events)))
    }

    async fn thread_resume(&self, request: ThreadResumeParams) -> Result<ThreadSummary, ApiError> {
        let summary = ThreadSummary {
            id: request.thread_id.clone(),
            backend: self.backend.clone(),
            cwd: request.cwd.clone(),
            additional_directories: request.additional_directories.clone(),
            title: None,
            updated_at: None,
            status: ThreadStatus::Active,
        };
        self.state.upsert_summary(summary.clone());
        {
            let mut sessions = self.state.sessions.lock().unwrap();
            let session = sessions
                .get_mut(&request.thread_id.0)
                .expect("summary was inserted");
            let replay = Turn {
                id: TurnId(format!("replay:{}", Uuid::new_v4())),
                status: TurnStatus::InProgress,
                items: Vec::new(),
                stop_reason: None,
                error: None,
            };
            session.active_turn = Some(replay.id.clone());
            session.turns.push(replay);
        }
        if let Err(error) = self
            .agent
            .resume_session(
                request.thread_id.0.clone(),
                request.cwd,
                request.additional_directories,
                Vec::new(),
                Some(v2::ReplayFrom::Start),
            )
            .await
        {
            self.state
                .sessions
                .lock()
                .unwrap()
                .remove(&request.thread_id.0);
            return Err(ApiError::new(error.to_string()));
        }
        let mut sessions = self.state.sessions.lock().unwrap();
        let session = sessions
            .get_mut(&request.thread_id.0)
            .expect("resumed session remains registered");
        if let Some(turn_id) = session.active_turn.take() {
            if let Some(turn) = session.turns.iter_mut().find(|turn| turn.id == turn_id) {
                turn.status = TurnStatus::Completed;
            }
        }
        session.summary.status = ThreadStatus::Idle;
        Ok(session.summary.clone())
    }

    async fn thread_unsubscribe(
        &self,
        _frontend: RpcContext<FrontendHandle>,
        request: ThreadUnsubscribeParams,
    ) -> Result<ThreadUnsubscribeResponse, ApiError> {
        let status = {
            let mut sessions = self.state.sessions.lock().unwrap();
            let Some(session) = sessions.get_mut(&request.thread_id.0) else {
                return Ok(ThreadUnsubscribeResponse {
                    status: ThreadUnsubscribeStatus::NotLoaded,
                });
            };
            if session.frontend.take().is_none() {
                return Ok(ThreadUnsubscribeResponse {
                    status: ThreadUnsubscribeStatus::NotSubscribed,
                });
            }
            session.summary.status = ThreadStatus::Idle;
            ThreadUnsubscribeStatus::Unsubscribed
        };
        if self.supports_close {
            self.agent
                .close(request.thread_id.0)
                .await
                .map_err(|e| ApiError::new(e.to_string()))?;
        }
        Ok(ThreadUnsubscribeResponse { status })
    }

    async fn thread_archive(
        &self,
        _request: ThreadArchiveParams,
    ) -> Result<ThreadArchiveResponse, ApiError> {
        Err(ApiError::with_code(
            "ACP does not support archiving sessions",
            "unsupported_operation",
        ))
    }

    async fn thread_delete(
        &self,
        _request: ThreadDeleteParams,
    ) -> Result<ThreadDeleteResponse, ApiError> {
        Err(ApiError::with_code(
            "stable ACP does not support deleting sessions",
            "unsupported_operation",
        ))
    }

    async fn turn_start(
        &self,
        frontend: RpcContext<FrontendHandle>,
        request: TurnStartParams,
    ) -> Result<TurnStartResponse, ApiError> {
        let turn_id = TurnId(Uuid::new_v4().to_string());
        let message_id = ItemId(
            request
                .client_user_message_id
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string()),
        );
        let turn = Turn {
            id: turn_id.clone(),
            status: TurnStatus::InProgress,
            items: vec![ThreadItem::UserMessage {
                id: message_id,
                content: request.input.clone(),
            }],
            stop_reason: None,
            error: None,
        };
        {
            let mut sessions = self.state.sessions.lock().unwrap();
            let session = sessions
                .get_mut(&request.thread_id.0)
                .ok_or_else(|| ApiError::new("unknown thread"))?;
            if session.active_turn.is_some() {
                return Err(ApiError::with_code(
                    "thread already has an active turn",
                    "thread_busy",
                ));
            }
            session.active_turn = Some(turn_id);
            session.frontend = Some(frontend.handle().clone());
            session.summary.status = ThreadStatus::Active;
            session.turns.push(turn.clone());
            Self::emit_turn_started(session, turn.clone());
        }
        spawn_prompt(
            self.agent.clone(),
            self.state.clone(),
            request.thread_id.0,
            turn.id.clone(),
            prompt_content(request.input),
        );
        Ok(TurnStartResponse { turn })
    }

    async fn turn_interrupt(&self, request: TurnInterruptParams) -> Result<(), ApiError> {
        let changed = {
            let mut queues = self.state.queues.queues.lock().unwrap();
            let queue = queues.entry(request.thread_id.0.clone()).or_default();
            if queue.submissions.is_empty() || queue.paused {
                false
            } else {
                queue.paused = true;
                queue.revision = queue.revision.wrapping_add(1);
                true
            }
        };
        if changed {
            let mut sessions = self.state.sessions.lock().unwrap();
            if let Some(session) = sessions.get_mut(&request.thread_id.0) {
                BridgeState::emit(
                    session,
                    ThreadEventKind::QueueChanged {
                        thread_id: request.thread_id.clone(),
                    },
                );
            }
        }
        self.agent
            .cancel(request.thread_id.0)
            .map_err(|e| ApiError::new(e.to_string()))
    }

    async fn thread_queue_add(
        &self,
        frontend: RpcContext<FrontendHandle>,
        request: ThreadQueueAddParams,
    ) -> Result<ThreadQueueAddResponse, ApiError> {
        let submission = QueuedSubmission {
            id: QueuedSubmissionId(Uuid::new_v4().to_string()),
            input: request.input,
            client_user_message_id: request.client_user_message_id,
        };
        let start_now = {
            let mut sessions = self.state.sessions.lock().unwrap();
            let session = sessions
                .get_mut(&request.thread_id.0)
                .ok_or_else(|| ApiError::new("unknown thread"))?;
            session.frontend = Some(frontend.handle().clone());
            let mut queues = self.state.queues.queues.lock().unwrap();
            let queue = queues.entry(request.thread_id.0.clone()).or_default();
            queue.submissions.push_back(submission.clone());
            queue.revision = queue.revision.wrapping_add(1);
            let next = if session.active_turn.is_none() && !queue.paused {
                let next = queue.submissions.pop_front();
                queue.revision = queue.revision.wrapping_add(1);
                next
            } else {
                None
            };
            BridgeState::emit(
                session,
                ThreadEventKind::QueueChanged {
                    thread_id: request.thread_id.clone(),
                },
            );
            next.map(|next| {
                let turn = turn_from_submission(&next);
                session.active_turn = Some(turn.id.clone());
                session.summary.status = ThreadStatus::Active;
                session.turns.push(turn.clone());
                Self::emit_turn_started(session, turn.clone());
                (turn.id, next.input)
            })
        };
        if let Some((turn_id, input)) = start_now {
            spawn_prompt(
                self.agent.clone(),
                self.state.clone(),
                request.thread_id.0,
                turn_id,
                prompt_content(input),
            );
        }
        Ok(ThreadQueueAddResponse {
            queued_submission: submission,
        })
    }

    async fn thread_queue_list(
        &self,
        request: ThreadQueueListParams,
    ) -> Result<ThreadQueueListResponse, ApiError> {
        if !self
            .state
            .sessions
            .lock()
            .unwrap()
            .contains_key(&request.thread_id.0)
        {
            return Err(ApiError::new("unknown thread"));
        }
        let queues = self.state.queues.queues.lock().unwrap();
        let empty = VecDeque::new();
        let (revision, submissions) = queues
            .get(&request.thread_id.0)
            .map(|queue| (queue.revision, &queue.submissions))
            .unwrap_or((0, &empty));
        let offset = request
            .cursor
            .as_deref()
            .map(|cursor| {
                let (cursor_revision, cursor_offset) = cursor.split_once(':').ok_or_else(|| {
                    ApiError::with_code("stale queue cursor", "stale_queue_cursor")
                })?;
                let cursor_revision = cursor_revision
                    .parse::<u64>()
                    .map_err(|_| ApiError::with_code("stale queue cursor", "stale_queue_cursor"))?;
                let cursor_offset = cursor_offset
                    .parse::<usize>()
                    .map_err(|_| ApiError::with_code("stale queue cursor", "stale_queue_cursor"))?;
                if cursor_revision != revision || cursor_offset > submissions.len() {
                    return Err(ApiError::with_code(
                        "stale queue cursor",
                        "stale_queue_cursor",
                    ));
                }
                Ok(cursor_offset)
            })
            .transpose()?
            .unwrap_or(0);
        let limit = request.limit.unwrap_or(100).max(1) as usize;
        let end = offset.saturating_add(limit).min(submissions.len());
        Ok(ThreadQueueListResponse {
            data: submissions.range(offset..end).cloned().collect(),
            next_cursor: (end < submissions.len()).then(|| format!("{revision}:{end}")),
        })
    }

    async fn thread_queue_update(
        &self,
        request: ThreadQueueUpdateParams,
    ) -> Result<ThreadQueueUpdateResponse, ApiError> {
        let submission = {
            let mut queues = self.state.queues.queues.lock().unwrap();
            let queue = queues.entry(request.thread_id.0.clone()).or_default();
            let submission = queue
                .submissions
                .iter_mut()
                .find(|submission| submission.id == request.queued_submission_id)
                .ok_or_else(|| {
                    ApiError::with_code(
                        "queued submission not found",
                        "queued_submission_not_found",
                    )
                })?;
            submission.input = request.input;
            let submission = submission.clone();
            queue.revision = queue.revision.wrapping_add(1);
            submission
        };
        self.emit_queue_changed(&request.thread_id)?;
        Ok(ThreadQueueUpdateResponse {
            queued_submission: submission,
        })
    }

    async fn thread_queue_delete(
        &self,
        request: ThreadQueueDeleteParams,
    ) -> Result<ThreadQueueDeleteResponse, ApiError> {
        let deleted = {
            let mut queues = self.state.queues.queues.lock().unwrap();
            let queue = queues.entry(request.thread_id.0.clone()).or_default();
            let position = queue
                .submissions
                .iter()
                .position(|submission| submission.id == request.queued_submission_id);
            if let Some(position) = position {
                queue.submissions.remove(position);
                queue.revision = queue.revision.wrapping_add(1);
                true
            } else {
                false
            }
        };
        if deleted {
            self.emit_queue_changed(&request.thread_id)?;
        }
        Ok(ThreadQueueDeleteResponse { deleted })
    }

    async fn thread_queue_reorder(
        &self,
        request: ThreadQueueReorderParams,
    ) -> Result<ThreadQueueReorderResponse, ApiError> {
        {
            let mut queues = self.state.queues.queues.lock().unwrap();
            let queue = queues.entry(request.thread_id.0.clone()).or_default();
            let existing: std::collections::HashSet<_> = queue
                .submissions
                .iter()
                .map(|submission| submission.id.clone())
                .collect();
            let requested: std::collections::HashSet<_> =
                request.queued_submission_ids.iter().cloned().collect();
            if existing.len() != request.queued_submission_ids.len() || existing != requested {
                return Err(ApiError::with_code(
                    "queued submission ids must be an exact permutation of the queue",
                    "invalid_queue_order",
                ));
            }
            let mut by_id: HashMap<_, _> = queue
                .submissions
                .drain(..)
                .map(|submission| (submission.id.clone(), submission))
                .collect();
            queue.submissions = request
                .queued_submission_ids
                .into_iter()
                .map(|id| by_id.remove(&id).expect("validated queue permutation"))
                .collect();
            queue.revision = queue.revision.wrapping_add(1);
        }
        self.emit_queue_changed(&request.thread_id)?;
        Ok(ThreadQueueReorderResponse {})
    }

    async fn thread_queue_start(
        &self,
        frontend: RpcContext<FrontendHandle>,
        request: ThreadQueueStartParams,
    ) -> Result<ThreadQueueStartResponse, ApiError> {
        let (turn, input) = {
            let mut sessions = self.state.sessions.lock().unwrap();
            let session = sessions
                .get_mut(&request.thread_id.0)
                .ok_or_else(|| ApiError::new("unknown thread"))?;
            if session.active_turn.is_some() {
                return Err(ApiError::with_code(
                    "thread already has an active turn",
                    "thread_busy",
                ));
            }
            session.frontend = Some(frontend.handle().clone());
            let mut queues = self.state.queues.queues.lock().unwrap();
            let queue = queues.entry(request.thread_id.0.clone()).or_default();
            let position = match request.queued_submission_id {
                Some(ref id) => queue
                    .submissions
                    .iter()
                    .position(|submission| &submission.id == id)
                    .ok_or_else(|| {
                        ApiError::with_code(
                            "queued submission not found",
                            "queued_submission_not_found",
                        )
                    })?,
                None => 0,
            };
            let submission = queue.submissions.remove(position).ok_or_else(|| {
                ApiError::with_code("thread queue is empty", "thread_queue_empty")
            })?;
            queue.paused = false;
            queue.revision = queue.revision.wrapping_add(1);
            let turn = turn_from_submission(&submission);
            session.active_turn = Some(turn.id.clone());
            session.summary.status = ThreadStatus::Active;
            session.turns.push(turn.clone());
            BridgeState::emit(
                session,
                ThreadEventKind::QueueChanged {
                    thread_id: request.thread_id.clone(),
                },
            );
            Self::emit_turn_started(session, turn.clone());
            (turn, submission.input)
        };
        spawn_prompt(
            self.agent.clone(),
            self.state.clone(),
            request.thread_id.0,
            turn.id.clone(),
            prompt_content(input),
        );
        Ok(ThreadQueueStartResponse { turn })
    }
}

impl AcpBridge {
    fn emit_turn_started(session: &mut SessionState, turn: Turn) {
        BridgeState::emit(session, ThreadEventKind::TurnStarted { turn });
    }

    fn emit_queue_changed(&self, thread_id: &ThreadId) -> Result<(), ApiError> {
        let mut sessions = self.state.sessions.lock().unwrap();
        let session = sessions
            .get_mut(&thread_id.0)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        BridgeState::emit(
            session,
            ThreadEventKind::QueueChanged {
                thread_id: thread_id.clone(),
            },
        );
        Ok(())
    }
}
