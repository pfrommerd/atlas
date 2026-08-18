use crate::{
    ApiError, Backend, BackendId, ContentBlock, Cursor, FrontendHandle, ItemId, QueuedSubmission,
    QueuedSubmissionId, Thread, ThreadArchiveParams, ThreadArchiveResponse, ThreadDeleteParams,
    ThreadDeleteResponse, ThreadEvent, ThreadEventKind, ThreadId, ThreadItem, ThreadListEvent,
    ThreadListParams, ThreadListResponse, ThreadQueueAddParams, ThreadQueueAddResponse,
    ThreadQueueDeleteParams, ThreadQueueDeleteResponse, ThreadQueueListParams,
    ThreadQueueListResponse, ThreadQueueReorderParams, ThreadQueueReorderResponse,
    ThreadQueueStartParams, ThreadQueueStartResponse, ThreadQueueUpdateParams,
    ThreadQueueUpdateResponse, ThreadReadParams, ThreadReadResponse, ThreadResumeParams,
    ThreadScope, ThreadSnapshot, ThreadStartParams, ThreadStartResponse, ThreadStatus,
    ThreadSubscribeParams, ThreadSummary, ThreadUnsubscribeParams, ThreadUnsubscribeResponse,
    ThreadUnsubscribeStatus, Turn, TurnId, TurnInterruptParams, TurnStartParams, TurnStartResponse,
    TurnStatus,
};
use atlas_rpc::{RpcContext, Stream};
use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const DEFAULT_SYSTEM_PROMPT: &str = "You are Atlas, a helpful AI assistant.";

/// Configuration for an OpenAI-compatible Chat Completions endpoint.
#[derive(Clone, Debug)]
pub struct ChatCompletionsConfig {
    pub base_url: String,
    pub model: String,
    pub api_key_env: Option<String>,
    pub headers: BTreeMap<String, String>,
}

impl ChatCompletionsConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            base_url: "https://api.openai.com/v1".into(),
            model: model.into(),
            api_key_env: Some("OPENAI_API_KEY".into()),
            headers: BTreeMap::new(),
        }
    }
}

/// Model transport used by the native agent. Future provider APIs are added here.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub enum NativeModelBackend {
    ChatCompletions(ChatCompletionsConfig),
}

#[derive(Clone, Debug)]
pub struct NativeAgentConfig {
    pub system_prompt: String,
    pub backend: NativeModelBackend,
}

impl NativeAgentConfig {
    pub fn new(backend: NativeModelBackend) -> Self {
        Self {
            system_prompt: DEFAULT_SYSTEM_PROMPT.into(),
            backend,
        }
    }
}

#[derive(Clone)]
pub struct NativeAgent {
    backend_id: BackendId,
    config: NativeAgentConfig,
    client: reqwest::Client,
    endpoint: String,
    state: NativeState,
}

#[derive(Clone)]
struct NativeState {
    threads: Arc<Mutex<HashMap<ThreadId, NativeThread>>>,
    list_events: broadcast::Sender<ThreadListEvent>,
}

struct NativeThread {
    summary: ThreadSummary,
    turns: Vec<Turn>,
    active_turn: Option<TurnId>,
    cancellation: Option<CancellationToken>,
    archived: bool,
    revision: u64,
    events: broadcast::Sender<ThreadEvent>,
    subscribers: HashSet<usize>,
    queue: NativeQueue,
}

#[derive(Default)]
struct NativeQueue {
    submissions: VecDeque<QueuedSubmission>,
    paused: bool,
    revision: u64,
}

enum CompletionResult {
    Completed { stop_reason: Option<String> },
    Interrupted,
    Failed(String),
}

impl NativeAgent {
    pub fn new(backend_id: BackendId, config: NativeAgentConfig) -> Result<Self, ApiError> {
        let (endpoint, headers) = match &config.backend {
            NativeModelBackend::ChatCompletions(chat) => {
                if chat.model.trim().is_empty() {
                    return Err(ApiError::new("Chat Completions model must not be empty"));
                }
                let endpoint = format!("{}/chat/completions", chat.base_url.trim_end_matches('/'));
                reqwest::Url::parse(&endpoint).map_err(|error| {
                    ApiError::new(format!("invalid Chat Completions URL: {error}"))
                })?;
                let mut headers = HeaderMap::new();
                for (name, value) in &chat.headers {
                    let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
                        ApiError::new(format!("invalid HTTP header name {name:?}: {error}"))
                    })?;
                    let value = HeaderValue::from_str(value).map_err(|error| {
                        ApiError::new(format!("invalid value for HTTP header {name}: {error}"))
                    })?;
                    headers.insert(name, value);
                }
                (endpoint, headers)
            }
        };
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(|error| ApiError::new(format!("failed to build HTTP client: {error}")))?;
        let (list_events, _) = broadcast::channel(256);
        Ok(Self {
            backend_id,
            config,
            client,
            endpoint,
            state: NativeState {
                threads: Arc::new(Mutex::new(HashMap::new())),
                list_events,
            },
        })
    }

    fn now() -> String {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .to_string()
    }

    fn emit(thread: &mut NativeThread, event: ThreadEventKind) {
        thread.revision = thread.revision.wrapping_add(1);
        let _ = thread.events.send(ThreadEvent {
            revision: thread.revision,
            event,
        });
    }

    fn validate_text_input(input: &[ContentBlock]) -> Result<(), ApiError> {
        if input
            .iter()
            .any(|block| !matches!(block, ContentBlock::Text { .. }))
        {
            return Err(ApiError::with_code(
                "the native Chat Completions backend currently accepts text input only",
                "unsupported_input",
            ));
        }
        Ok(())
    }

    fn new_turn(input: Vec<ContentBlock>, client_message_id: Option<String>) -> Turn {
        Turn {
            id: TurnId(Uuid::new_v4().to_string()),
            status: TurnStatus::InProgress,
            items: vec![ThreadItem::UserMessage {
                id: ItemId(client_message_id.unwrap_or_else(|| Uuid::new_v4().to_string())),
                content: input,
            }],
            stop_reason: None,
            error: None,
        }
    }

    fn start_turn_locked(
        &self,
        thread: &mut NativeThread,
        input: Vec<ContentBlock>,
        client_message_id: Option<String>,
    ) -> Result<(Turn, CancellationToken), ApiError> {
        Self::validate_text_input(&input)?;
        if thread.archived {
            return Err(ApiError::with_code("thread is archived", "thread_archived"));
        }
        if thread.active_turn.is_some() {
            return Err(ApiError::with_code(
                "thread already has an active turn",
                "thread_busy",
            ));
        }
        let turn = Self::new_turn(input, client_message_id);
        let cancellation = CancellationToken::new();
        thread.active_turn = Some(turn.id.clone());
        thread.cancellation = Some(cancellation.clone());
        thread.summary.status = ThreadStatus::Active;
        thread.summary.updated_at = Some(Self::now());
        thread.turns.push(turn.clone());
        Self::emit(thread, ThreadEventKind::TurnStarted { turn: turn.clone() });
        Ok((turn, cancellation))
    }

    fn spawn_completion(
        &self,
        thread_id: ThreadId,
        turn_id: TurnId,
        cancellation: CancellationToken,
    ) {
        let agent = self.clone();
        tokio::spawn(async move {
            let result = agent
                .stream_completion(&thread_id, &turn_id, cancellation)
                .await;
            agent.finish_turn(thread_id, turn_id, result);
        });
    }

    async fn stream_completion(
        &self,
        thread_id: &ThreadId,
        turn_id: &TurnId,
        cancellation: CancellationToken,
    ) -> CompletionResult {
        let messages = {
            let threads = self.state.threads.lock().unwrap();
            let Some(thread) = threads.get(thread_id) else {
                return CompletionResult::Failed("thread was deleted".into());
            };
            let mut messages = vec![json!({
                "role": "system",
                "content": self.config.system_prompt,
            })];
            for turn in &thread.turns {
                if turn.status == TurnStatus::Failed || turn.status == TurnStatus::Interrupted {
                    continue;
                }
                for item in &turn.items {
                    let (role, content) = match item {
                        ThreadItem::UserMessage { content, .. } => ("user", content),
                        ThreadItem::AgentMessage { content, .. } => ("assistant", content),
                        _ => continue,
                    };
                    let text = content
                        .iter()
                        .filter_map(|block| match block {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    messages.push(json!({"role": role, "content": text}));
                }
            }
            messages
        };
        let NativeModelBackend::ChatCompletions(chat) = &self.config.backend;
        let mut request = self.client.post(&self.endpoint).json(&json!({
            "model": chat.model,
            "messages": messages,
            "stream": true,
        }));
        if let Some(variable) = &chat.api_key_env {
            let key = match std::env::var(variable) {
                Ok(key) if !key.is_empty() => key,
                _ => {
                    return CompletionResult::Failed(format!(
                        "API key environment variable {variable} is not set"
                    ));
                }
            };
            request = request.bearer_auth(key);
        }
        let response = tokio::select! {
            _ = cancellation.cancelled() => return CompletionResult::Interrupted,
            response = request.send() => match response {
                Ok(response) => response,
                Err(error) => return CompletionResult::Failed(format!("Chat Completions request failed: {error}")),
            }
        };
        if !response.status().is_success() {
            let status = response.status();
            let body = tokio::select! {
                _ = cancellation.cancelled() => return CompletionResult::Interrupted,
                body = response.text() => body.unwrap_or_default(),
            };
            let body = body.trim();
            return CompletionResult::Failed(if body.is_empty() {
                format!("Chat Completions request returned HTTP {status}")
            } else {
                format!("Chat Completions request returned HTTP {status}: {body}")
            });
        }

        let item_id = ItemId(Uuid::new_v4().to_string());
        let mut text = String::new();
        let mut item_started = false;
        let mut stop_reason = None;
        let mut buffer = String::new();
        let mut data_lines = Vec::new();
        let mut bytes = response.bytes_stream();
        loop {
            let chunk = tokio::select! {
                _ = cancellation.cancelled() => return CompletionResult::Interrupted,
                chunk = bytes.next() => chunk,
            };
            let Some(chunk) = chunk else { break };
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    return CompletionResult::Failed(format!(
                        "Chat Completions stream failed: {error}"
                    ));
                }
            };
            let chunk = match std::str::from_utf8(&chunk) {
                Ok(chunk) => chunk,
                Err(error) => {
                    return CompletionResult::Failed(format!(
                        "Chat Completions stream was not UTF-8: {error}"
                    ));
                }
            };
            buffer.push_str(chunk);
            while let Some(newline) = buffer.find('\n') {
                let mut line = buffer[..newline].to_owned();
                buffer.drain(..=newline);
                if line.ends_with('\r') {
                    line.pop();
                }
                if let Some(data) = line.strip_prefix("data:") {
                    data_lines.push(data.trim_start().to_owned());
                    continue;
                }
                if !line.is_empty() || data_lines.is_empty() {
                    continue;
                }
                let data = data_lines.join("\n");
                data_lines.clear();
                if data == "[DONE]" {
                    break;
                }
                let value: Value = match serde_json::from_str(&data) {
                    Ok(value) => value,
                    Err(error) => {
                        return CompletionResult::Failed(format!(
                            "invalid Chat Completions event: {error}"
                        ));
                    }
                };
                if let Some(reason) = value
                    .pointer("/choices/0/finish_reason")
                    .and_then(Value::as_str)
                {
                    stop_reason = Some(reason.to_owned());
                }
                if let (Some(input_tokens), Some(output_tokens)) = (
                    value
                        .pointer("/usage/prompt_tokens")
                        .and_then(Value::as_u64),
                    value
                        .pointer("/usage/completion_tokens")
                        .and_then(Value::as_u64),
                ) {
                    let mut threads = self.state.threads.lock().unwrap();
                    if let Some(thread) = threads.get_mut(thread_id) {
                        Self::emit(
                            thread,
                            ThreadEventKind::UsageUpdated {
                                input_tokens,
                                output_tokens,
                            },
                        );
                    }
                }
                let Some(delta) = value
                    .pointer("/choices/0/delta/content")
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                text.push_str(delta);
                let item = ThreadItem::AgentMessage {
                    id: item_id.clone(),
                    content: vec![ContentBlock::Text { text: text.clone() }],
                };
                let mut threads = self.state.threads.lock().unwrap();
                let Some(thread) = threads.get_mut(thread_id) else {
                    return CompletionResult::Interrupted;
                };
                if thread.active_turn.as_ref() != Some(turn_id) {
                    return CompletionResult::Interrupted;
                }
                let Some(turn) = thread.turns.iter_mut().find(|turn| &turn.id == turn_id) else {
                    return CompletionResult::Interrupted;
                };
                if item_started {
                    if let Some(existing) = turn
                        .items
                        .iter_mut()
                        .find(|existing| existing.id() == &item_id)
                    {
                        *existing = item.clone();
                    }
                } else {
                    turn.items.push(item.clone());
                }
                Self::emit(
                    thread,
                    if std::mem::replace(&mut item_started, true) {
                        ThreadEventKind::ItemUpdated {
                            turn_id: turn_id.clone(),
                            item,
                        }
                    } else {
                        ThreadEventKind::ItemStarted {
                            turn_id: turn_id.clone(),
                            item,
                        }
                    },
                );
            }
        }
        if !data_lines.is_empty() {
            return CompletionResult::Failed("Chat Completions stream ended mid-event".into());
        }
        let item = ThreadItem::AgentMessage {
            id: item_id.clone(),
            content: vec![ContentBlock::Text { text }],
        };
        let mut threads = self.state.threads.lock().unwrap();
        if let Some(thread) = threads.get_mut(thread_id)
            && let Some(turn) = thread.turns.iter_mut().find(|turn| &turn.id == turn_id)
        {
            if item_started {
                if let Some(existing) = turn
                    .items
                    .iter_mut()
                    .find(|existing| existing.id() == &item_id)
                {
                    *existing = item.clone();
                }
            } else {
                turn.items.push(item.clone());
            }
            Self::emit(
                thread,
                ThreadEventKind::ItemCompleted {
                    turn_id: turn_id.clone(),
                    item,
                },
            );
        }
        CompletionResult::Completed { stop_reason }
    }

    fn finish_turn(&self, thread_id: ThreadId, turn_id: TurnId, result: CompletionResult) {
        let next = {
            let mut threads = self.state.threads.lock().unwrap();
            let Some(thread) = threads.get_mut(&thread_id) else {
                return;
            };
            if thread.active_turn.as_ref() != Some(&turn_id) {
                return;
            }
            thread.active_turn = None;
            thread.cancellation = None;
            thread.summary.status = ThreadStatus::Idle;
            thread.summary.updated_at = Some(Self::now());
            let mut reported_error = None;
            let event = {
                let Some(turn) = thread.turns.iter_mut().find(|turn| turn.id == turn_id) else {
                    return;
                };
                match result {
                    CompletionResult::Completed { stop_reason } => {
                        turn.status = TurnStatus::Completed;
                        turn.stop_reason = stop_reason;
                        ThreadEventKind::TurnCompleted { turn: turn.clone() }
                    }
                    CompletionResult::Interrupted => {
                        turn.status = TurnStatus::Interrupted;
                        turn.stop_reason = Some("cancelled".into());
                        ThreadEventKind::TurnCompleted { turn: turn.clone() }
                    }
                    CompletionResult::Failed(error) => {
                        turn.status = TurnStatus::Failed;
                        turn.error = Some(error.clone());
                        reported_error = Some(error);
                        ThreadEventKind::TurnFailed { turn: turn.clone() }
                    }
                }
            };
            if let Some(error) = reported_error {
                thread.queue.paused = true;
                Self::emit_queue_changed(thread);
                Self::emit(thread, ThreadEventKind::Error { message: error });
            }
            Self::emit(thread, event);
            let _ = self.state.list_events.send(ThreadListEvent::Updated {
                thread: thread.summary.clone(),
            });
            if thread.queue.paused {
                None
            } else {
                let submission = thread.queue.submissions.pop_front();
                if submission.is_some() {
                    thread.queue.revision = thread.queue.revision.wrapping_add(1);
                    Self::emit(
                        thread,
                        ThreadEventKind::QueueChanged {
                            thread_id: thread_id.clone(),
                        },
                    );
                }
                submission.and_then(|submission| {
                    self.start_turn_locked(
                        thread,
                        submission.input,
                        Some(submission.client_user_message_id),
                    )
                    .ok()
                })
            }
        };
        if let Some((turn, cancellation)) = next {
            self.spawn_completion(thread_id, turn.id, cancellation);
        }
    }

    fn emit_queue_changed(thread: &mut NativeThread) {
        thread.queue.revision = thread.queue.revision.wrapping_add(1);
        Self::emit(
            thread,
            ThreadEventKind::QueueChanged {
                thread_id: thread.summary.id.clone(),
            },
        );
    }
}

impl Backend for NativeAgent {
    async fn thread_start(
        &self,
        request: ThreadStartParams,
    ) -> Result<ThreadStartResponse, ApiError> {
        let id = ThreadId(Uuid::new_v4().to_string());
        let summary = ThreadSummary {
            id: id.clone(),
            backend: self.backend_id.clone(),
            cwd: request.cwd,
            additional_directories: request.additional_directories,
            title: None,
            updated_at: Some(Self::now()),
            status: ThreadStatus::Idle,
        };
        let (events, _) = broadcast::channel(512);
        self.state.threads.lock().unwrap().insert(
            id,
            NativeThread {
                summary: summary.clone(),
                turns: Vec::new(),
                active_turn: None,
                cancellation: None,
                archived: false,
                revision: 0,
                events,
                subscribers: HashSet::new(),
                queue: NativeQueue::default(),
            },
        );
        let _ = self.state.list_events.send(ThreadListEvent::Added {
            thread: summary.clone(),
        });
        Ok(ThreadStartResponse { thread: summary })
    }

    async fn thread_list(
        &self,
        request: ThreadListParams,
    ) -> Result<(ThreadListResponse, Stream<ThreadListEvent>), ApiError> {
        let mut threads: Vec<_> = self
            .state
            .threads
            .lock()
            .unwrap()
            .values()
            .filter(|thread| match request.scope {
                ThreadScope::Active => !thread.archived,
                ThreadScope::Archived => thread.archived,
                ThreadScope::All => true,
            })
            .map(|thread| thread.summary.clone())
            .collect();
        if let Some(search) = request
            .search_term
            .as_ref()
            .map(|search| search.to_lowercase())
        {
            threads.retain(|thread| {
                format!("{}\n{}", thread.title.as_deref().unwrap_or(""), thread.cwd)
                    .to_lowercase()
                    .contains(&search)
            });
        }
        threads.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.id.cmp(&right.id))
        });
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
        let end = offset
            .saturating_add(request.limit.unwrap_or(100).clamp(1, 100))
            .min(threads.len());
        let page = ThreadListResponse {
            threads: threads[offset..end].to_vec(),
            next_cursor: (end < threads.len()).then(|| Cursor(end.to_string())),
        };
        let stream = if request.subscribe {
            let events =
                tokio_stream::wrappers::BroadcastStream::new(self.state.list_events.subscribe())
                    .filter_map(|event| async { event.ok() });
            Stream::new(events)
        } else {
            Stream::new(futures_util::stream::empty())
        };
        Ok((page, stream))
    }

    async fn thread_read(&self, request: ThreadReadParams) -> Result<ThreadReadResponse, ApiError> {
        let threads = self.state.threads.lock().unwrap();
        let thread = threads
            .get(&request.thread_id)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        let end = request
            .before
            .as_ref()
            .map(|cursor| cursor.0.parse::<usize>())
            .transpose()
            .map_err(|_| ApiError::new("stale thread cursor"))?
            .unwrap_or(thread.turns.len())
            .min(thread.turns.len());
        let start = end.saturating_sub(request.limit.unwrap_or(20).clamp(1, 100));
        Ok(ThreadReadResponse {
            thread: Thread {
                summary: thread.summary.clone(),
                turns: thread.turns[start..end].to_vec(),
            },
            older_cursor: (start > 0).then(|| Cursor(start.to_string())),
        })
    }

    async fn thread_subscribe(
        &self,
        frontend: RpcContext<FrontendHandle>,
        request: ThreadSubscribeParams,
    ) -> Result<(ThreadSnapshot, Stream<ThreadEvent>), ApiError> {
        let mut threads = self.state.threads.lock().unwrap();
        let thread = threads
            .get_mut(&request.thread_id)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        thread.subscribers.insert(frontend.connection_id());
        let start = thread
            .turns
            .len()
            .saturating_sub(request.tail_turns.clamp(1, 100));
        let snapshot = ThreadSnapshot {
            revision: thread.revision,
            thread: Thread {
                summary: thread.summary.clone(),
                turns: thread.turns[start..].to_vec(),
            },
            older_cursor: (start > 0).then(|| Cursor(start.to_string())),
        };
        let events = tokio_stream::wrappers::BroadcastStream::new(thread.events.subscribe())
            .filter_map(|event| async { event.ok() });
        Ok((snapshot, Stream::new(events)))
    }

    async fn thread_resume(&self, request: ThreadResumeParams) -> Result<ThreadSummary, ApiError> {
        let mut threads = self.state.threads.lock().unwrap();
        let thread = threads
            .get_mut(&request.thread_id)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        if thread.archived {
            return Err(ApiError::with_code("thread is archived", "thread_archived"));
        }
        thread.summary.cwd = request.cwd;
        thread.summary.additional_directories = request.additional_directories;
        thread.summary.updated_at = Some(Self::now());
        let summary = thread.summary.clone();
        Self::emit(
            thread,
            ThreadEventKind::ThreadUpdated {
                thread: summary.clone(),
            },
        );
        let _ = self.state.list_events.send(ThreadListEvent::Updated {
            thread: summary.clone(),
        });
        Ok(summary)
    }

    async fn thread_unsubscribe(
        &self,
        frontend: RpcContext<FrontendHandle>,
        request: ThreadUnsubscribeParams,
    ) -> Result<ThreadUnsubscribeResponse, ApiError> {
        let mut threads = self.state.threads.lock().unwrap();
        let Some(thread) = threads.get_mut(&request.thread_id) else {
            return Ok(ThreadUnsubscribeResponse {
                status: ThreadUnsubscribeStatus::NotLoaded,
            });
        };
        Ok(ThreadUnsubscribeResponse {
            status: if thread.subscribers.remove(&frontend.connection_id()) {
                ThreadUnsubscribeStatus::Unsubscribed
            } else {
                ThreadUnsubscribeStatus::NotSubscribed
            },
        })
    }

    async fn thread_archive(
        &self,
        request: ThreadArchiveParams,
    ) -> Result<ThreadArchiveResponse, ApiError> {
        let mut threads = self.state.threads.lock().unwrap();
        let thread = threads
            .get_mut(&request.thread_id)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        if thread.active_turn.is_some() {
            return Err(ApiError::with_code(
                "cannot archive a thread with an active turn",
                "thread_busy",
            ));
        }
        thread.archived = true;
        let _ = self.state.list_events.send(ThreadListEvent::Archived {
            thread_id: request.thread_id,
        });
        Ok(ThreadArchiveResponse {})
    }

    async fn thread_delete(
        &self,
        request: ThreadDeleteParams,
    ) -> Result<ThreadDeleteResponse, ApiError> {
        let mut threads = self.state.threads.lock().unwrap();
        if threads
            .get(&request.thread_id)
            .is_some_and(|thread| thread.active_turn.is_some())
        {
            return Err(ApiError::with_code(
                "cannot delete a thread with an active turn",
                "thread_busy",
            ));
        }
        threads
            .remove(&request.thread_id)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        let _ = self.state.list_events.send(ThreadListEvent::Deleted {
            thread_id: request.thread_id,
        });
        Ok(ThreadDeleteResponse {})
    }

    async fn turn_start(
        &self,
        _frontend: RpcContext<FrontendHandle>,
        request: TurnStartParams,
    ) -> Result<TurnStartResponse, ApiError> {
        let (turn, cancellation) = {
            let mut threads = self.state.threads.lock().unwrap();
            let thread = threads
                .get_mut(&request.thread_id)
                .ok_or_else(|| ApiError::new("unknown thread"))?;
            self.start_turn_locked(thread, request.input, request.client_user_message_id)?
        };
        self.spawn_completion(request.thread_id, turn.id.clone(), cancellation);
        Ok(TurnStartResponse { turn })
    }

    async fn turn_interrupt(&self, request: TurnInterruptParams) -> Result<(), ApiError> {
        let mut threads = self.state.threads.lock().unwrap();
        let thread = threads
            .get_mut(&request.thread_id)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        if let Some(expected) = request.turn_id
            && thread.active_turn.as_ref() != Some(&expected)
        {
            return Err(ApiError::with_code("turn is not active", "turn_not_active"));
        }
        let cancellation = thread
            .cancellation
            .clone()
            .ok_or_else(|| ApiError::with_code("thread has no active turn", "turn_not_active"))?;
        thread.queue.paused = true;
        Self::emit_queue_changed(thread);
        cancellation.cancel();
        Ok(())
    }

    async fn thread_queue_add(
        &self,
        _frontend: RpcContext<FrontendHandle>,
        request: ThreadQueueAddParams,
    ) -> Result<ThreadQueueAddResponse, ApiError> {
        Self::validate_text_input(&request.input)?;
        let submission = QueuedSubmission {
            id: QueuedSubmissionId(Uuid::new_v4().to_string()),
            input: request.input,
            client_user_message_id: request.client_user_message_id,
        };
        let start = {
            let mut threads = self.state.threads.lock().unwrap();
            let thread = threads
                .get_mut(&request.thread_id)
                .ok_or_else(|| ApiError::new("unknown thread"))?;
            if thread.archived {
                return Err(ApiError::with_code("thread is archived", "thread_archived"));
            }
            thread.queue.submissions.push_back(submission.clone());
            Self::emit_queue_changed(thread);
            if thread.active_turn.is_none() && !thread.queue.paused {
                let next = thread
                    .queue
                    .submissions
                    .pop_front()
                    .expect("submission was just queued");
                Self::emit_queue_changed(thread);
                Some(self.start_turn_locked(
                    thread,
                    next.input,
                    Some(next.client_user_message_id),
                )?)
            } else {
                None
            }
        };
        if let Some((turn, cancellation)) = start {
            self.spawn_completion(request.thread_id, turn.id, cancellation);
        }
        Ok(ThreadQueueAddResponse {
            queued_submission: submission,
        })
    }

    async fn thread_queue_list(
        &self,
        request: ThreadQueueListParams,
    ) -> Result<ThreadQueueListResponse, ApiError> {
        let threads = self.state.threads.lock().unwrap();
        let thread = threads
            .get(&request.thread_id)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        let offset = request
            .cursor
            .as_deref()
            .map(|cursor| {
                let (revision, offset) = cursor.split_once(':').ok_or_else(|| {
                    ApiError::with_code("stale queue cursor", "stale_queue_cursor")
                })?;
                let revision = revision
                    .parse::<u64>()
                    .map_err(|_| ApiError::with_code("stale queue cursor", "stale_queue_cursor"))?;
                let offset = offset
                    .parse::<usize>()
                    .map_err(|_| ApiError::with_code("stale queue cursor", "stale_queue_cursor"))?;
                if revision != thread.queue.revision || offset > thread.queue.submissions.len() {
                    return Err(ApiError::with_code(
                        "stale queue cursor",
                        "stale_queue_cursor",
                    ));
                }
                Ok(offset)
            })
            .transpose()?
            .unwrap_or(0);
        let end = offset
            .saturating_add(request.limit.unwrap_or(100).max(1) as usize)
            .min(thread.queue.submissions.len());
        Ok(ThreadQueueListResponse {
            data: thread
                .queue
                .submissions
                .range(offset..end)
                .cloned()
                .collect(),
            next_cursor: (end < thread.queue.submissions.len())
                .then(|| format!("{}:{end}", thread.queue.revision)),
        })
    }

    async fn thread_queue_update(
        &self,
        request: ThreadQueueUpdateParams,
    ) -> Result<ThreadQueueUpdateResponse, ApiError> {
        Self::validate_text_input(&request.input)?;
        let mut threads = self.state.threads.lock().unwrap();
        let thread = threads
            .get_mut(&request.thread_id)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        let submission = thread
            .queue
            .submissions
            .iter_mut()
            .find(|submission| submission.id == request.queued_submission_id)
            .ok_or_else(|| {
                ApiError::with_code("queued submission not found", "queued_submission_not_found")
            })?;
        submission.input = request.input;
        let submission = submission.clone();
        Self::emit_queue_changed(thread);
        Ok(ThreadQueueUpdateResponse {
            queued_submission: submission,
        })
    }

    async fn thread_queue_delete(
        &self,
        request: ThreadQueueDeleteParams,
    ) -> Result<ThreadQueueDeleteResponse, ApiError> {
        let mut threads = self.state.threads.lock().unwrap();
        let thread = threads
            .get_mut(&request.thread_id)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        let position = thread
            .queue
            .submissions
            .iter()
            .position(|submission| submission.id == request.queued_submission_id);
        let deleted = position
            .and_then(|position| thread.queue.submissions.remove(position))
            .is_some();
        if deleted {
            Self::emit_queue_changed(thread);
        }
        Ok(ThreadQueueDeleteResponse { deleted })
    }

    async fn thread_queue_reorder(
        &self,
        request: ThreadQueueReorderParams,
    ) -> Result<ThreadQueueReorderResponse, ApiError> {
        let mut threads = self.state.threads.lock().unwrap();
        let thread = threads
            .get_mut(&request.thread_id)
            .ok_or_else(|| ApiError::new("unknown thread"))?;
        let existing: HashSet<_> = thread
            .queue
            .submissions
            .iter()
            .map(|submission| submission.id.clone())
            .collect();
        let requested: HashSet<_> = request.queued_submission_ids.iter().cloned().collect();
        if existing.len() != request.queued_submission_ids.len() || existing != requested {
            return Err(ApiError::with_code(
                "queued submission ids must be an exact permutation of the queue",
                "invalid_queue_order",
            ));
        }
        let mut by_id: HashMap<_, _> = thread
            .queue
            .submissions
            .drain(..)
            .map(|submission| (submission.id.clone(), submission))
            .collect();
        thread.queue.submissions = request
            .queued_submission_ids
            .into_iter()
            .map(|id| by_id.remove(&id).expect("validated queue permutation"))
            .collect();
        Self::emit_queue_changed(thread);
        Ok(ThreadQueueReorderResponse {})
    }

    async fn thread_queue_start(
        &self,
        _frontend: RpcContext<FrontendHandle>,
        request: ThreadQueueStartParams,
    ) -> Result<ThreadQueueStartResponse, ApiError> {
        let (turn, cancellation) = {
            let mut threads = self.state.threads.lock().unwrap();
            let thread = threads
                .get_mut(&request.thread_id)
                .ok_or_else(|| ApiError::new("unknown thread"))?;
            if thread.active_turn.is_some() {
                return Err(ApiError::with_code(
                    "thread already has an active turn",
                    "thread_busy",
                ));
            }
            let position = match &request.queued_submission_id {
                Some(id) => thread
                    .queue
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
            let submission = thread.queue.submissions.remove(position).ok_or_else(|| {
                ApiError::with_code("thread queue is empty", "thread_queue_empty")
            })?;
            thread.queue.paused = false;
            Self::emit_queue_changed(thread);
            self.start_turn_locked(
                thread,
                submission.input,
                Some(submission.client_user_message_id),
            )?
        };
        self.spawn_completion(request.thread_id, turn.id.clone(), cancellation);
        Ok(ThreadQueueStartResponse { turn })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApprovalRequest, ApprovalResponse, BackendHandle, Frontend};
    use atlas_rpc::{InProcessTransport, Peer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    struct TestFrontend;

    impl Frontend for TestFrontend {
        async fn request_approval(
            &self,
            _request: ApprovalRequest,
        ) -> Result<ApprovalResponse, ApiError> {
            Ok(ApprovalResponse::Cancelled)
        }
    }

    fn agent(base_url: String) -> NativeAgent {
        let mut chat = ChatCompletionsConfig::new("test-model");
        chat.base_url = base_url;
        chat.api_key_env = None;
        chat.headers.insert("x-atlas-test".into(), "present".into());
        NativeAgent::new(
            BackendId("native".into()),
            NativeAgentConfig::new(NativeModelBackend::ChatCompletions(chat)),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn streams_chat_completions_into_backend_events() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(str::trim)
                            .map(str::to_owned)
                    })
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            request_tx
                .send(String::from_utf8(request).unwrap())
                .unwrap();
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"hello \"},\"finish_reason\":null}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"world\"},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });

        let agent = agent(format!("http://{address}/v1"));
        let (caller_transport, receiver_transport) = InProcessTransport::pair();
        let caller = Peer::new(caller_transport);
        let receiver = Peer::new(receiver_transport);
        receiver.register::<BackendHandle, _>(agent);
        caller.register::<FrontendHandle, _>(TestFrontend);
        let backend = BackendHandle::new(caller);
        let started = backend
            .thread_start(ThreadStartParams {
                cwd: "/workspace".into(),
                additional_directories: Vec::new(),
                backend: None,
            })
            .await
            .unwrap();
        let (_, mut events) = backend
            .thread_subscribe(ThreadSubscribeParams {
                thread_id: started.thread.id.clone(),
                tail_turns: 20,
            })
            .await
            .unwrap();
        backend
            .turn_start(TurnStartParams {
                thread_id: started.thread.id.clone(),
                input: vec![ContentBlock::Text { text: "hi".into() }],
                client_user_message_id: Some("user-message".into()),
            })
            .await
            .unwrap();

        let mut completed = None;
        while let Some(event) =
            tokio::time::timeout(std::time::Duration::from_secs(2), events.next())
                .await
                .unwrap()
        {
            if let ThreadEventKind::TurnCompleted { turn } = event.unwrap().event {
                completed = Some(turn);
                break;
            }
        }
        let completed = completed.expect("completion event");
        assert_eq!(completed.status, TurnStatus::Completed);
        assert_eq!(completed.stop_reason.as_deref(), Some("stop"));
        assert!(matches!(
            completed.items.last(),
            Some(ThreadItem::AgentMessage { content, .. })
                if content == &vec![ContentBlock::Text { text: "hello world".into() }]
        ));

        let request = request_rx.await.unwrap();
        assert!(request.starts_with("POST /v1/chat/completions HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("x-atlas-test: present")
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["model"], "test-model");
        assert_eq!(body["messages"][0]["content"], DEFAULT_SYSTEM_PROMPT);
        assert_eq!(body["messages"][1]["content"], "hi");
        assert_eq!(body["stream"], true);
    }

    #[tokio::test]
    async fn in_memory_archive_scopes_and_text_validation_work() {
        let agent = agent("http://127.0.0.1:1/v1".into());
        let started = agent
            .thread_start(ThreadStartParams {
                cwd: "/work/needle".into(),
                additional_directories: Vec::new(),
                backend: None,
            })
            .await
            .unwrap();
        let (active, _) = agent
            .thread_list(ThreadListParams {
                scope: ThreadScope::Active,
                search_term: Some("needle".into()),
                cursor: None,
                limit: None,
                subscribe: false,
            })
            .await
            .unwrap();
        assert_eq!(active.threads.len(), 1);
        agent
            .thread_archive(ThreadArchiveParams {
                thread_id: started.thread.id.clone(),
            })
            .await
            .unwrap();
        let (archived, _) = agent
            .thread_list(ThreadListParams {
                scope: ThreadScope::Archived,
                search_term: None,
                cursor: None,
                limit: None,
                subscribe: false,
            })
            .await
            .unwrap();
        assert_eq!(archived.threads.len(), 1);
        assert_eq!(
            NativeAgent::validate_text_input(&[ContentBlock::Image {
                uri: "https://example.test/image.png".into(),
                mime_type: Some("image/png".into()),
            }])
            .unwrap_err()
            .code
            .as_deref(),
            Some("unsupported_input")
        );
    }
}
