//! Local Unix-socket daemon which exposes Atlas RPC services and proxies ACP.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use atlas_acp::latest::{self, Agent, AgentHandle, Client, ClientHandle};
use atlas_acp::AcpError;
use atlas_rpc::{JsonTransport, Peer, RpcContext};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio_util::codec::{Framed, LinesCodec};

use crate::protocol::{
    Atlas, AtlasHandle, SessionListEvent, SessionListRequest, SessionPage, SessionScope,
    SessionSubscription,
};

#[derive(Deserialize)]
struct Config {
    agent: AgentConfig,
}

#[derive(Deserialize)]
struct AgentConfig {
    command: String,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Clone)]
struct Daemon {
    agent: AgentHandle,
    cache: SessionCache,
    events: broadcast::Sender<SessionListEvent>,
    clients: Arc<Mutex<HashMap<String, Vec<ClientHandle>>>>,
    connected: Arc<AtomicUsize>,
    shutdown: Arc<tokio::sync::Notify>,
}

#[derive(Clone)]
struct SessionCache {
    state: Arc<Mutex<SessionCacheState>>,
    ready: Arc<tokio::sync::Notify>,
}

struct SessionCacheState {
    sessions: HashMap<String, latest::SessionInfo>,
    active: HashSet<String>,
    revision: u64,
    loading: bool,
    error: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SessionCursor {
    revision: u64,
    scope: SessionScope,
    filter: Option<String>,
    offset: usize,
}

impl SessionCache {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SessionCacheState {
                sessions: HashMap::new(),
                active: HashSet::new(),
                revision: 0,
                loading: true,
                error: None,
            })),
            ready: Arc::new(tokio::sync::Notify::new()),
        }
    }

    async fn populate(&self, agent: AgentHandle) {
        let mut cursor = None;
        loop {
            match agent.list_sessions(None, cursor).await {
                Ok(page) => {
                    {
                        let mut state = self.state.lock().unwrap();
                        for session in page.sessions {
                            state.sessions.insert(session.session_id.clone(), session);
                        }
                        state.revision = state.revision.wrapping_add(1);
                    }
                    match page.next_cursor {
                        Some(next) => cursor = Some(next),
                        None => break,
                    }
                }
                Err(error) => {
                    let mut state = self.state.lock().unwrap();
                    state.loading = false;
                    state.error = Some(error.to_string());
                    self.ready.notify_waiters();
                    return;
                }
            }
        }
        self.state.lock().unwrap().loading = false;
        self.ready.notify_waiters();
    }

    async fn wait_ready(&self) -> Result<(), AcpError> {
        loop {
            let waiting = {
                let state = self.state.lock().unwrap();
                if !state.loading {
                    return state
                        .error
                        .clone()
                        .map_or(Ok(()), |error| Err(AcpError::new(error)));
                }
                self.ready.notified()
            };
            waiting.await;
        }
    }

    fn upsert(&self, session: latest::SessionInfo) {
        let mut state = self.state.lock().unwrap();
        state.sessions.insert(session.session_id.clone(), session);
        state.revision = state.revision.wrapping_add(1);
    }

    fn set_active(&self, id: &str, active: bool) -> Option<latest::SessionInfo> {
        let mut state = self.state.lock().unwrap();
        if active {
            state.active.insert(id.to_owned());
        } else {
            state.active.remove(id);
        }
        state.revision = state.revision.wrapping_add(1);
        state
            .sessions
            .get(id)
            .cloned()
            .map(|session| Self::decorate(&state, session))
    }

    fn decorate(
        state: &SessionCacheState,
        mut session: latest::SessionInfo,
    ) -> latest::SessionInfo {
        let mut meta = session.meta.take().unwrap_or_else(|| json!({}));
        if let Some(object) = meta.as_object_mut() {
            object.insert("atlas".into(), json!({"lifecycle": if state.active.contains(&session.session_id) { "active" } else { "inactive" }}));
        }
        session.meta = Some(meta);
        session
    }

    fn active_empty(&self) -> bool {
        self.state.lock().unwrap().active.is_empty()
    }

    async fn page(&self, request: &SessionListRequest) -> Result<SessionPage, AcpError> {
        if request.scope == SessionScope::All {
            self.wait_ready().await?;
        }
        let state = self.state.lock().unwrap();
        let filter = request.filter.as_ref().map(|filter| filter.to_lowercase());
        let offset = match request.cursor.as_deref() {
            Some(cursor) => {
                let cursor: SessionCursor = serde_json::from_str(cursor)
                    .map_err(|_| AcpError::new("invalid session list cursor"))?;
                if cursor.revision != state.revision
                    || cursor.scope != request.scope
                    || cursor.filter != filter
                {
                    return Err(AcpError::new("stale session list cursor"));
                }
                cursor.offset
            }
            None => 0,
        };
        let mut sessions: Vec<_> = state
            .sessions
            .values()
            .cloned()
            .filter(|session| {
                (request.scope == SessionScope::All || state.active.contains(&session.session_id))
                    && filter.as_ref().is_none_or(|filter| {
                        format!(
                            "{}\n{}\n{}",
                            session.title.as_deref().unwrap_or(""),
                            session.cwd,
                            session.session_id
                        )
                        .to_lowercase()
                        .contains(filter)
                    })
            })
            .collect();
        sessions.sort_by(|left, right| {
            state
                .active
                .contains(&right.session_id)
                .cmp(&state.active.contains(&left.session_id))
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| left.session_id.cmp(&right.session_id))
        });
        let limit = request.limit.unwrap_or(100).clamp(1, 100);
        let total = sessions.len();
        let next_offset = offset.saturating_add(limit);
        let page = sessions
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|session| Self::decorate(&state, session))
            .collect();
        let next_cursor = if next_offset < total {
            Some(
                serde_json::to_string(&SessionCursor {
                    revision: state.revision,
                    scope: request.scope,
                    filter,
                    offset: next_offset,
                })
                .expect("cursor serializes"),
            )
        } else {
            None
        };
        Ok(SessionPage {
            sessions: page,
            next_cursor,
        })
    }
}

pub fn default_socket() -> io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    Ok(runtime.join("atlas").join("atlas.sock"))
}

fn config_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("atlas/config.toml"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config/atlas/config.toml"))
}

pub async fn serve(socket: &Path) -> io::Result<()> {
    let source = std::fs::read_to_string(config_path()?)?;
    let config: Config = toml::from_str(&source)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        match tokio::net::UnixStream::connect(socket).await {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "Atlas daemon already owns the socket",
                ))
            }
            Err(_) => std::fs::remove_file(socket)?,
        }
    }
    let listener = UnixListener::bind(socket)?;

    let mut child = Command::new(&config.agent.command)
        .args(&config.agent.args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let transport = StdioTransport::new(stdin, stdout);
    let downstream = Peer::new(JsonTransport(transport));
    let (events, _) = broadcast::channel(64);
    let clients = Arc::new(Mutex::new(HashMap::new()));
    let agent = atlas_acp::initialize(
        downstream,
        DownstreamClient(clients.clone()),
        atlas_acp::InitializeRequest {
            protocol_version: latest::PROTOCOL_VERSION,
            info: latest::Implementation {
                name: "atlas".into(),
                title: "Atlas".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            },
            capabilities: latest::Capabilities::default(),
        },
    )
    .await
    .map_err(|error| io::Error::other(error.to_string()))?;
    let daemon = Daemon {
        agent,
        cache: SessionCache::new(),
        events,
        clients,
        connected: Arc::new(AtomicUsize::new(0)),
        shutdown: Arc::new(tokio::sync::Notify::new()),
    };
    let cache = daemon.cache.clone();
    let agent = daemon.agent.clone();
    tokio::spawn(async move { cache.populate(agent).await });
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let daemon = daemon.clone();
                    daemon.connected.fetch_add(1, Ordering::Relaxed);
                    tokio::spawn(async move {
                        let peer = Peer::new(JsonTransport(Framed::new(stream, LinesCodec::new())));
                        peer.register::<AgentHandle, _>(daemon.clone());
                        peer.register::<AtlasHandle, _>(daemon.clone());
                        peer.closed().await;
                        daemon.connected.fetch_sub(1, Ordering::Relaxed);
                        daemon.stop_if_idle();
                    });
                }
                Err(error) => return Err(error),
            },
            _ = daemon.shutdown.notified() => break,
        }
    }
    let _ = child.kill().await;
    if let Err(error) = std::fs::remove_file(socket) {
        if error.kind() != io::ErrorKind::NotFound {
            return Err(error);
        }
    }
    Ok(())
}

impl Daemon {
    fn publish(&self, event: SessionListEvent) {
        let _ = self.events.send(event);
    }

    fn stop_if_idle(&self) {
        if self.connected.load(Ordering::Relaxed) == 0 && self.cache.active_empty() {
            self.shutdown.notify_waiters();
        }
    }
}

impl Agent for Daemon {
    async fn new_session(
        &self,
        cwd: String,
        additional_directories: Vec<String>,
        mcp_servers: Vec<serde_json::Value>,
    ) -> Result<latest::NewSessionResponse, AcpError> {
        let response = self
            .agent
            .new_session(cwd.clone(), additional_directories.clone(), mcp_servers)
            .await
            .map_err(|e| AcpError::new(e.to_string()))?;
        let session = latest::SessionInfo {
            session_id: response.session_id.clone(),
            cwd,
            additional_directories,
            title: None,
            updated_at: None,
            meta: None,
        };
        self.cache.upsert(session);
        if let Some(session) = self.cache.set_active(&response.session_id, true) {
            self.publish(SessionListEvent::Added { session });
        }
        Ok(response)
    }
    async fn list_sessions(
        &self,
        cwd: Option<String>,
        cursor: Option<String>,
    ) -> Result<latest::ListSessionsResponse, AcpError> {
        self.cache.wait_ready().await?;
        let page = self
            .cache
            .page(&SessionListRequest {
                scope: SessionScope::All,
                filter: cwd,
                cursor,
                limit: Some(100),
                deltas: false,
            })
            .await?;
        Ok(latest::ListSessionsResponse {
            sessions: page.sessions,
            next_cursor: page.next_cursor,
        })
    }
    async fn resume_session(
        &self,
        session_id: latest::SessionId,
        cwd: String,
        additional_directories: Vec<String>,
        mcp_servers: Vec<serde_json::Value>,
    ) -> Result<latest::ResumeSessionResponse, AcpError> {
        let active_session_id = session_id.clone();
        let response = self
            .agent
            .resume_session(session_id, cwd, additional_directories, mcp_servers)
            .await
            .map_err(|e| AcpError::new(e.to_string()))?;
        if let Some(session) = self.cache.set_active(&active_session_id, true) {
            self.publish(SessionListEvent::Updated { session });
        }
        Ok(response)
    }
    async fn prompt(
        &self,
        client: RpcContext<ClientHandle>,
        session_id: latest::SessionId,
        prompt: Vec<serde_json::Value>,
    ) -> Result<(), AcpError> {
        self.clients
            .lock()
            .unwrap()
            .entry(session_id.clone())
            .or_default()
            .push(client.handle().clone());
        let agent = self.agent.clone();
        tokio::spawn(async move {
            let _ = agent.prompt(session_id, prompt).await;
        });
        Ok(())
    }
    async fn cancel(&self, session_id: latest::SessionId) -> Result<(), AcpError> {
        self.agent
            .cancel(session_id)
            .map_err(|e| AcpError::new(e.to_string()))
    }
    async fn close(&self, session_id: latest::SessionId) -> Result<(), AcpError> {
        let response = self
            .agent
            .close(session_id.clone())
            .await
            .map_err(|e| AcpError::new(e.to_string()))?;
        self.cache.set_active(&session_id, false);
        self.publish(SessionListEvent::Removed { session_id });
        self.stop_if_idle();
        Ok(response)
    }
}

impl Atlas for Daemon {
    async fn list_sessions(
        &self,
        request: SessionListRequest,
    ) -> Result<(SessionPage, atlas_rpc::Stream<SessionListEvent>), AcpError> {
        let page = self.cache.page(&request).await?;
        if !request.deltas {
            return Ok((page, atlas_rpc::Stream::new(futures_util::stream::empty())));
        }
        let scope = request.scope;
        let updates = tokio_stream::wrappers::BroadcastStream::new(self.events.subscribe())
            .filter_map(move |event| async move {
                let event = event.ok()?;
                let include = match &event {
                    SessionListEvent::Removed { .. } => true,
                    SessionListEvent::Added { session } | SessionListEvent::Updated { session } => {
                        scope == SessionScope::All
                            || session
                                .meta
                                .as_ref()
                                .and_then(|meta| meta.get("atlas"))
                                .and_then(|atlas| atlas.get("lifecycle"))
                                .and_then(|value| value.as_str())
                                == Some("active")
                    }
                    SessionListEvent::Snapshot { .. } => true,
                };
                include.then_some(event)
            });
        Ok((page, atlas_rpc::Stream::new(updates)))
    }
    async fn subscribe(&self, _: SessionSubscription) -> Result<(), AcpError> {
        Ok(())
    }
    async fn unsubscribe(&self, _: SessionSubscription) -> Result<(), AcpError> {
        Ok(())
    }
}

#[derive(Clone)]
struct DownstreamClient(Arc<Mutex<HashMap<String, Vec<ClientHandle>>>>);
impl Client for DownstreamClient {
    async fn session_update(
        &self,
        session_id: latest::SessionId,
        update: serde_json::Value,
    ) -> Result<(), AcpError> {
        if let Some(clients) = self.0.lock().unwrap().get(&session_id).cloned() {
            for client in clients {
                let _ = client.session_update(session_id.clone(), update.clone());
            }
        }
        Ok(())
    }
    async fn request_permission(
        &self,
        _: latest::SessionId,
        _: String,
        _: Option<String>,
        _: Vec<serde_json::Value>,
    ) -> Result<latest::PermissionResponse, AcpError> {
        Err(AcpError::new(
            "no subscribed TUI can answer this permission request yet",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> latest::SessionInfo {
        latest::SessionInfo {
            session_id: id.into(),
            cwd: format!("/{id}"),
            additional_directories: Vec::new(),
            title: Some(id.into()),
            updated_at: Some(format!("2026-01-0{id}T00:00:00Z")),
            meta: None,
        }
    }

    #[tokio::test]
    async fn active_pages_do_not_wait_for_history_loading() {
        let cache = SessionCache::new();
        cache.upsert(session("1"));
        cache.set_active("1", true);
        let page = cache
            .page(&SessionListRequest {
                scope: SessionScope::Active,
                filter: None,
                cursor: None,
                limit: Some(20),
                deltas: false,
            })
            .await
            .unwrap();
        assert_eq!(page.sessions.len(), 1);
    }

    #[tokio::test]
    async fn cached_pages_filter_and_continue_with_a_cursor() {
        let cache = SessionCache::new();
        cache.upsert(session("1"));
        cache.upsert(session("2"));
        cache.upsert(session("3"));
        cache.state.lock().unwrap().loading = false;
        let request = SessionListRequest {
            scope: SessionScope::All,
            filter: Some("/".into()),
            cursor: None,
            limit: Some(2),
            deltas: false,
        };
        let first = cache.page(&request).await.unwrap();
        assert_eq!(first.sessions.len(), 2);
        let second = cache
            .page(&SessionListRequest {
                cursor: first.next_cursor,
                ..request
            })
            .await
            .unwrap();
        assert_eq!(second.sessions.len(), 1);
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
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "agent stdin closed"))
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
