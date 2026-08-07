use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use atlas_acp::latest::{self, AgentHandle, Client, ClientHandle};
use atlas_acp::AcpError;
use atlas_rpc::{JsonTransport, Peer};
use futures_util::StreamExt;
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LinesCodec};

use crate::daemon;
use crate::protocol::{AtlasHandle, SessionFilter, SessionListEvent, SessionListRequest};

#[derive(Clone)]
struct TuiClient(Sender<latest::SessionUpdate>);

impl Client for TuiClient {
    async fn session_update(&self, update: latest::SessionUpdate) -> Result<(), AcpError> {
        let _ = self.0.send(update);
        Ok(())
    }

    async fn cancel_request(&self, _: serde_json::Value) -> Result<(), AcpError> {
        Ok(())
    }

    async fn request_permission(
        &self,
        _: latest::PermissionRequest,
    ) -> Result<latest::PermissionResponse, AcpError> {
        Err(AcpError::new(
            "permission prompts are not rendered by the TUI yet",
        ))
    }
}

/// The Atlas daemon connection owned by the TUI's Tokio runtime.
#[derive(Clone)]
pub struct DaemonClient {
    _connection: std::sync::Arc<Connection>,
    agent: AgentHandle,
    events: std::sync::Arc<std::sync::Mutex<Receiver<SessionListEvent>>>,
    updates: std::sync::Arc<std::sync::Mutex<Receiver<latest::SessionUpdate>>>,
}

struct Connection {
    peer: Peer,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.peer.disconnect();
    }
}

impl DaemonClient {
    pub async fn connect_or_start() -> io::Result<(Self, Vec<latest::SessionInfo>)> {
        let socket = daemon::default_socket()?;
        let stream = match UnixStream::connect(&socket).await {
            Ok(stream) => Ok(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                let executable = std::env::current_exe()?;
                std::process::Command::new(executable)
                    .arg("serve")
                    .arg("--socket")
                    .arg(&socket)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()?;
                let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
                loop {
                    match UnixStream::connect(&socket).await {
                        Ok(stream) => break Ok(stream),
                        Err(connect_error) if tokio::time::Instant::now() < deadline => {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                            if connect_error.kind() != io::ErrorKind::NotFound
                                && connect_error.kind() != io::ErrorKind::ConnectionRefused
                            {
                                break Err(connect_error);
                            }
                        }
                        Err(connect_error) => break Err(connect_error),
                    }
                }
            }
            Err(error) => Err(error),
        }?;
        let peer = Peer::new(JsonTransport(Framed::new(stream, LinesCodec::new())));
        let agent = AgentHandle::new(peer.clone());
        let atlas = AtlasHandle::new(peer.clone());
        let (update_tx, update_rx) = mpsc::channel();
        peer.register::<ClientHandle, _>(TuiClient(update_tx));
        let (sessions, mut stream) = atlas
            .list_sessions(SessionListRequest {
                filter: SessionFilter::Active,
                deltas: true,
            })
            .await
            .map_err(|error| io::Error::other(error.to_string()))?;
        let (event_tx, event_rx) = mpsc::channel();
        tokio::spawn(async move {
            while let Some(event) = stream.next().await {
                if let Ok(event) = event {
                    if event_tx.send(event).is_err() {
                        break;
                    }
                } else {
                    break;
                }
            }
        });
        Ok((
            Self {
                _connection: std::sync::Arc::new(Connection { peer }),
                agent,
                events: std::sync::Arc::new(std::sync::Mutex::new(event_rx)),
                updates: std::sync::Arc::new(std::sync::Mutex::new(update_rx)),
            },
            sessions,
        ))
    }

    pub fn drain_events(&self) -> Vec<SessionListEvent> {
        let receiver = self.events.lock().unwrap();
        receiver.try_iter().collect()
    }

    pub fn drain_updates(&self) -> Vec<latest::SessionUpdate> {
        let receiver = self.updates.lock().unwrap();
        receiver.try_iter().collect()
    }

    pub async fn new_session(&self, cwd: String) -> Result<String, String> {
        self.agent
            .new_session(latest::NewSessionRequest {
                cwd,
                additional_directories: Vec::new(),
                mcp_servers: Vec::new(),
            })
            .await
            .map(|response| response.session_id)
            .map_err(|error| error.to_string())
    }

    pub async fn close(&self, session_id: String) -> Result<(), String> {
        self.agent
            .close(latest::SessionRequest { session_id })
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub async fn prompt(&self, session_id: String, text: String) -> Result<(), String> {
        self.agent
            .prompt(latest::PromptRequest {
                session_id,
                prompt: vec![serde_json::json!({"type": "text", "text": text})],
            })
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    pub async fn resume(
        &self,
        session_id: String,
        cwd: String,
        additional_directories: Vec<String>,
    ) -> Result<(), String> {
        self.agent
            .resume_session(latest::ResumeSessionRequest {
                session_id,
                cwd,
                additional_directories,
                mcp_servers: Vec::new(),
            })
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}
