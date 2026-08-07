//! Local Unix-socket daemon which exposes Atlas RPC services and proxies ACP.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use atlas_acp::latest::{self, Agent, AgentHandle, Client, ClientHandle};
use atlas_acp::AcpError;
use atlas_rpc::{JsonTransport, Peer, RpcContext};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::sync::broadcast;
use tokio_util::codec::{Framed, LinesCodec};

use crate::protocol::{
    Atlas, AtlasHandle, Empty, SessionFilter, SessionListEvent, SessionListRequest,
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
    sessions: Arc<Mutex<HashMap<String, latest::SessionInfo>>>,
    active: Arc<Mutex<HashSet<String>>>,
    events: broadcast::Sender<SessionListEvent>,
    clients: Arc<Mutex<HashMap<String, Vec<ClientHandle>>>>,
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
        std::fs::remove_file(socket)?;
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
    let agent = AgentHandle::new(downstream.clone());
    let (events, _) = broadcast::channel(64);
    let daemon = Daemon {
        agent,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        active: Arc::new(Mutex::new(HashSet::new())),
        events,
        clients: Arc::new(Mutex::new(HashMap::new())),
    };
    downstream.register::<ClientHandle, _>(DownstreamClient(daemon.clone()));
    while let Ok((stream, _)) = listener.accept().await {
        let daemon = daemon.clone();
        tokio::spawn(async move {
            let peer = Peer::new(JsonTransport(Framed::new(stream, LinesCodec::new())));
            peer.register::<AgentHandle, _>(daemon.clone());
            peer.register::<AtlasHandle, _>(daemon);
        });
    }
    Ok(())
}

impl Daemon {
    fn decorate(&self, mut session: latest::SessionInfo) -> latest::SessionInfo {
        let active = self.active.lock().unwrap().contains(&session.session_id);
        let mut meta = session.meta.take().unwrap_or_else(|| json!({}));
        if let Some(object) = meta.as_object_mut() {
            object.insert(
                "atlas".into(),
                json!({"lifecycle": if active { "active" } else { "inactive" }}),
            );
        }
        session.meta = Some(meta);
        session
    }

    fn publish(&self, event: SessionListEvent) {
        let _ = self.events.send(event);
    }
}

impl Agent for Daemon {
    async fn initialize(
        &self,
        request: latest::InitializeRequest,
    ) -> Result<latest::InitializeResponse, AcpError> {
        self.agent
            .initialize(request)
            .await
            .map_err(|e| AcpError::new(e.to_string()))
    }
    async fn new_session(
        &self,
        request: latest::NewSessionRequest,
    ) -> Result<latest::NewSessionResponse, AcpError> {
        let response = self
            .agent
            .new_session(request)
            .await
            .map_err(|e| AcpError::new(e.to_string()))?;
        self.active
            .lock()
            .unwrap()
            .insert(response.session_id.clone());
        Ok(response)
    }
    async fn list_sessions(
        &self,
        request: latest::ListSessionsRequest,
    ) -> Result<latest::ListSessionsResponse, AcpError> {
        let response = self
            .agent
            .list_sessions(request)
            .await
            .map_err(|e| AcpError::new(e.to_string()))?;
        let sessions = response
            .sessions
            .into_iter()
            .map(|session| {
                let session = self.decorate(session);
                self.sessions
                    .lock()
                    .unwrap()
                    .insert(session.session_id.clone(), session.clone());
                session
            })
            .collect();
        Ok(latest::ListSessionsResponse {
            sessions,
            next_cursor: response.next_cursor,
        })
    }
    async fn resume_session(
        &self,
        request: latest::ResumeSessionRequest,
    ) -> Result<latest::ResumeSessionResponse, AcpError> {
        let response = self
            .agent
            .resume_session(request.clone())
            .await
            .map_err(|e| AcpError::new(e.to_string()))?;
        self.active
            .lock()
            .unwrap()
            .insert(request.session_id.clone());
        if let Some(session) = self
            .sessions
            .lock()
            .unwrap()
            .get(&request.session_id)
            .cloned()
        {
            self.publish(SessionListEvent::Updated {
                session: self.decorate(session),
            });
        }
        Ok(response)
    }
    async fn prompt(
        &self,
        client: RpcContext<ClientHandle>,
        request: latest::PromptRequest,
    ) -> Result<latest::Empty, AcpError> {
        self.clients
            .lock()
            .unwrap()
            .entry(request.session_id.clone())
            .or_default()
            .push(client.handle().clone());
        self.agent
            .prompt(request)
            .await
            .map_err(|e| AcpError::new(e.to_string()))
    }
    async fn cancel(&self, request: latest::SessionRequest) -> Result<(), AcpError> {
        self.agent
            .cancel(request)
            .map_err(|e| AcpError::new(e.to_string()))
    }
    async fn close(&self, request: latest::SessionRequest) -> Result<latest::Empty, AcpError> {
        let response = self
            .agent
            .close(request.clone())
            .await
            .map_err(|e| AcpError::new(e.to_string()))?;
        self.active.lock().unwrap().remove(&request.session_id);
        self.publish(SessionListEvent::Removed {
            session_id: request.session_id,
        });
        Ok(response)
    }
}

impl Atlas for Daemon {
    async fn list_sessions(
        &self,
        request: SessionListRequest,
    ) -> Result<
        (
            Vec<latest::SessionInfo>,
            atlas_rpc::Stream<SessionListEvent>,
        ),
        AcpError,
    > {
        let response = self
            .agent
            .list_sessions(latest::ListSessionsRequest::default())
            .await
            .map_err(|error| AcpError::new(error.to_string()))?;
        {
            let mut sessions = self.sessions.lock().unwrap();
            for session in response.sessions {
                sessions.insert(session.session_id.clone(), session);
            }
        }
        let sessions = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .cloned()
            .filter(|session| {
                request.filter == SessionFilter::All
                    || self.active.lock().unwrap().contains(&session.session_id)
            })
            .map(|session| self.decorate(session))
            .collect();
        if !request.deltas {
            return Ok((
                sessions,
                atlas_rpc::Stream::new(futures_util::stream::empty()),
            ));
        }
        let filter = request.filter;
        let active = self.active.clone();
        let updates = tokio_stream::wrappers::BroadcastStream::new(self.events.subscribe())
            .filter_map(move |event| {
                let active = active.clone();
                async move {
                    let event = event.ok()?;
                    let include = match &event {
                        SessionListEvent::Removed { .. } => filter == SessionFilter::Active,
                        SessionListEvent::Added { session }
                        | SessionListEvent::Updated { session } => {
                            filter == SessionFilter::All
                                || active.lock().unwrap().contains(&session.session_id)
                        }
                        SessionListEvent::Snapshot { .. } => true,
                    };
                    include.then_some(event)
                }
            });
        Ok((sessions, atlas_rpc::Stream::new(updates)))
    }
    async fn subscribe(&self, _: SessionSubscription) -> Result<Empty, AcpError> {
        Ok(Empty {})
    }
    async fn unsubscribe(&self, _: SessionSubscription) -> Result<Empty, AcpError> {
        Ok(Empty {})
    }
}

#[derive(Clone)]
struct DownstreamClient(Daemon);
impl Client for DownstreamClient {
    async fn session_update(&self, update: latest::SessionUpdate) -> Result<(), AcpError> {
        if let Some(clients) = self
            .0
            .clients
            .lock()
            .unwrap()
            .get(&update.session_id)
            .cloned()
        {
            for client in clients {
                let _ = client.session_update(update.clone());
            }
        }
        Ok(())
    }
    async fn cancel_request(&self, _: Value) -> Result<(), AcpError> {
        Ok(())
    }
    async fn request_permission(
        &self,
        _: latest::PermissionRequest,
    ) -> Result<latest::PermissionResponse, AcpError> {
        Err(AcpError::new(
            "no subscribed TUI can answer this permission request yet",
        ))
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
