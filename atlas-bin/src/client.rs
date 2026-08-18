use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use atlas_agent::{
    ApiError, ApprovalRequest, ApprovalResponse, BackendHandle, ContentBlock, Cursor, Frontend,
    FrontendHandle, QueuedSubmission, QueuedSubmissionId, ThreadArchiveParams, ThreadDeleteParams,
    ThreadEvent, ThreadId, ThreadListEvent, ThreadListParams, ThreadQueueAddParams,
    ThreadQueueDeleteParams, ThreadQueueDeleteResponse, ThreadQueueListParams,
    ThreadQueueReorderParams, ThreadQueueStartParams, ThreadQueueStartResponse,
    ThreadQueueUpdateParams, ThreadQueueUpdateResponse, ThreadReadParams, ThreadReadResponse,
    ThreadResumeParams, ThreadScope, ThreadSnapshot, ThreadStartParams, ThreadSubscribeParams,
    ThreadSummary, ThreadUnsubscribeParams, ThreadUnsubscribeStatus, TurnInterruptParams,
    TurnStartParams,
};
use atlas_rpc::{CallError, Peer};
use atlas_swarm::{
    PathResource, SwarmPath,
    auth::UserSigner,
    connect_remote_service_with_agent,
    local::{ResolveServiceRequest, StateSelector, StateSnapshot},
};
use futures_util::StreamExt;

#[derive(Clone)]
struct TuiFrontend {
    approvals: Sender<PendingApproval>,
}
impl Frontend for TuiFrontend {
    async fn request_approval(
        &self,
        request: ApprovalRequest,
    ) -> Result<ApprovalResponse, ApiError> {
        let (response, receive) = tokio::sync::oneshot::channel();
        self.approvals
            .send(PendingApproval {
                request,
                response: Some(response),
            })
            .map_err(|_| ApiError::new("the TUI disconnected while approval was pending"))?;
        receive
            .await
            .map_err(|_| ApiError::new("the approval dialogue was closed"))
    }
}

pub struct PendingApproval {
    pub request: ApprovalRequest,
    response: Option<tokio::sync::oneshot::Sender<ApprovalResponse>>,
}

impl PendingApproval {
    pub fn respond(mut self, response: ApprovalResponse) {
        if let Some(sender) = self.response.take() {
            let _ = sender.send(response);
        }
    }
}

#[derive(Clone)]
pub struct DaemonClient {
    _connection: std::sync::Arc<Connection>,
    backend: BackendHandle,
    events: std::sync::Arc<std::sync::Mutex<Receiver<ThreadListEvent>>>,
    thread_events: std::sync::Arc<std::sync::Mutex<Receiver<(ThreadId, u64, ThreadEvent)>>>,
    thread_event_tx: Sender<(ThreadId, u64, ThreadEvent)>,
    watch_sequence: std::sync::Arc<AtomicU64>,
    watchers: std::sync::Arc<std::sync::Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    approvals: std::sync::Arc<std::sync::Mutex<Receiver<PendingApproval>>>,
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
    pub async fn connect_or_start(reset: bool) -> io::Result<(Self, Vec<ThreadSummary>)> {
        let daemon = crate::bundle::extract()?;
        let signer = UserSigner::discover().await?;
        let control = atlas_daemon::connect_or_start(&daemon, reset).await?;
        let StateSnapshot::Paths(paths) = control
            .query(StateSelector::Paths {
                prefix: Some(SwarmPath::new("/nodes").unwrap()),
            })
            .await
            .map_err(io::Error::other)?
        else {
            unreachable!()
        };
        let endpoint_id = control.endpoint_id(()).await.map_err(io::Error::other)?;
        let node_path = paths.into_iter().find_map(|(path, entry)| matches!(entry.resource, Some(PathResource::Node(ref node)) if node.endpoint_id == endpoint_id).then_some(path)).ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "local swarm node is unavailable"))?;
        let node = node_path.as_str().rsplit('/').next().unwrap();
        let path = SwarmPath::new(format!("/atlas/{node}")).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let resolution = loop {
            match control
                .resolve_service(ResolveServiceRequest { path: path.clone() })
                .await
            {
                Ok(value) => break value,
                Err(_) if tokio::time::Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(50)).await
                }
                Err(error) => return Err(io::Error::other(error)),
            }
        };
        let peer = connect_remote_service_with_agent(resolution.endpoint_addr, &path, &signer)
            .await
            .map_err(io::Error::other)?;
        let backend = BackendHandle::new(peer.clone());
        let (approval_tx, approval_rx) = mpsc::channel();
        peer.register::<FrontendHandle, _>(TuiFrontend {
            approvals: approval_tx,
        });
        let (page, mut stream) = backend
            .thread_list(ThreadListParams {
                scope: ThreadScope::All,
                search_term: None,
                cursor: None,
                limit: None,
                subscribe: true,
            })
            .await
            .map_err(|e| io::Error::other(e.to_string()))?;
        let (event_tx, event_rx) = mpsc::channel();
        tokio::spawn(async move {
            while let Some(Ok(event)) = stream.next().await {
                if event_tx.send(event).is_err() {
                    break;
                }
            }
        });
        let (thread_event_tx, thread_event_rx) = mpsc::channel();
        Ok((
            Self {
                _connection: std::sync::Arc::new(Connection { peer }),
                backend,
                events: std::sync::Arc::new(std::sync::Mutex::new(event_rx)),
                thread_events: std::sync::Arc::new(std::sync::Mutex::new(thread_event_rx)),
                thread_event_tx,
                watch_sequence: std::sync::Arc::new(AtomicU64::new(0)),
                watchers: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
                approvals: std::sync::Arc::new(std::sync::Mutex::new(approval_rx)),
            },
            page.threads,
        ))
    }
    pub fn drain_events(&self) -> Vec<ThreadListEvent> {
        self.events.lock().unwrap().try_iter().collect()
    }
    pub fn drain_thread_events(&self) -> Vec<(ThreadId, u64, ThreadEvent)> {
        self.thread_events.lock().unwrap().try_iter().collect()
    }
    pub fn drain_approvals(&self) -> Vec<PendingApproval> {
        self.approvals.lock().unwrap().try_iter().collect()
    }
    pub async fn new_thread(&self, cwd: String) -> Result<String, String> {
        self.backend
            .thread_start(ThreadStartParams {
                cwd,
                additional_directories: Vec::new(),
                backend: None,
            })
            .await
            .map(|r| r.thread.id.0)
            .map_err(|e| e.to_string())
    }
    pub async fn unsubscribe(&self, id: String) -> Result<ThreadUnsubscribeStatus, String> {
        let response = self
            .backend
            .thread_unsubscribe(ThreadUnsubscribeParams {
                thread_id: ThreadId(id.clone()),
            })
            .await
            .map_err(|e| e.to_string())?;
        if let Some(watcher) = self.watchers.lock().unwrap().remove(&id) {
            watcher.abort();
        }
        Ok(response.status)
    }
    pub async fn archive(&self, id: String) -> Result<(), String> {
        self.backend
            .thread_archive(ThreadArchiveParams {
                thread_id: ThreadId(id),
            })
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    pub async fn delete(&self, id: String) -> Result<(), String> {
        self.backend
            .thread_delete(ThreadDeleteParams {
                thread_id: ThreadId(id),
            })
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    pub async fn prompt(&self, id: String, text: String, queue: bool) -> Result<(), String> {
        let client_user_message_id = uuid::Uuid::new_v4().to_string();
        if queue {
            return self
                .queue_add(
                    id,
                    vec![ContentBlock::Text { text }],
                    client_user_message_id,
                )
                .await
                .map(|_| ());
        }
        let result = self
            .backend
            .turn_start(TurnStartParams {
                thread_id: ThreadId(id.clone()),
                input: vec![ContentBlock::Text { text: text.clone() }],
                client_user_message_id: Some(client_user_message_id.clone()),
            })
            .await;
        match result {
            Ok(_) => Ok(()),
            Err(CallError::Rpc(error))
                if error
                    .data
                    .as_ref()
                    .and_then(|data| data.get("code"))
                    .and_then(|value| value.as_str())
                    == Some("thread_busy") =>
            {
                self.queue_add(
                    id,
                    vec![ContentBlock::Text { text }],
                    client_user_message_id,
                )
                .await
                .map(|_| ())
            }
            Err(error) => Err(error.to_string()),
        }
    }
    pub async fn resume(
        &self,
        id: String,
        cwd: String,
        additional_directories: Vec<String>,
    ) -> Result<(), String> {
        self.backend
            .thread_resume(ThreadResumeParams {
                thread_id: ThreadId(id),
                cwd,
                additional_directories,
            })
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    pub async fn interrupt(&self, id: String) -> Result<(), String> {
        self.backend
            .turn_interrupt(TurnInterruptParams {
                thread_id: ThreadId(id),
                turn_id: None,
            })
            .await
            .map_err(|e| e.to_string())
    }
    pub async fn list_threads(
        &self,
        search_term: Option<String>,
        cursor: Option<Cursor>,
    ) -> Result<(Vec<ThreadSummary>, Option<Cursor>), String> {
        self.backend
            .thread_list(ThreadListParams {
                scope: ThreadScope::All,
                search_term,
                cursor,
                limit: Some(20),
                subscribe: false,
            })
            .await
            .map(|(p, _)| (p.threads, p.next_cursor))
            .map_err(|e| e.to_string())
    }
    pub async fn watch_thread(
        &self,
        id: String,
    ) -> Result<(ThreadSnapshot, u64, Vec<QueuedSubmission>), String> {
        let thread_id = ThreadId(id.clone());
        let (snapshot, mut stream) = self
            .backend
            .thread_subscribe(ThreadSubscribeParams {
                thread_id: thread_id.clone(),
                tail_turns: 20,
            })
            .await
            .map_err(|e| e.to_string())?;
        let watch = self.watch_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        if let Some(previous) = self.watchers.lock().unwrap().remove(&id) {
            previous.abort();
        }
        let events = self.thread_event_tx.clone();
        let event_thread_id = thread_id.clone();
        let task = tokio::spawn(async move {
            while let Some(Ok(event)) = stream.next().await {
                if events
                    .send((event_thread_id.clone(), watch, event))
                    .is_err()
                {
                    break;
                }
            }
        });
        self.watchers.lock().unwrap().insert(id, task);
        let queue = self.queue_list(thread_id.0.clone()).await?;
        Ok((snapshot, watch, queue))
    }

    pub async fn queue_add(
        &self,
        id: String,
        input: Vec<ContentBlock>,
        client_user_message_id: String,
    ) -> Result<QueuedSubmission, String> {
        self.backend
            .thread_queue_add(ThreadQueueAddParams {
                thread_id: ThreadId(id),
                input,
                client_user_message_id,
            })
            .await
            .map(|response| response.queued_submission)
            .map_err(|e| e.to_string())
    }

    pub async fn queue_list(&self, id: String) -> Result<Vec<QueuedSubmission>, String> {
        let mut data = Vec::new();
        let mut cursor = None;
        loop {
            let response = self
                .backend
                .thread_queue_list(ThreadQueueListParams {
                    thread_id: ThreadId(id.clone()),
                    cursor,
                    limit: None,
                })
                .await
                .map_err(|e| e.to_string())?;
            data.extend(response.data);
            let Some(next) = response.next_cursor else {
                break;
            };
            cursor = Some(next);
        }
        Ok(data)
    }

    pub async fn queue_update(
        &self,
        id: String,
        queued_submission_id: QueuedSubmissionId,
        input: Vec<ContentBlock>,
    ) -> Result<ThreadQueueUpdateResponse, String> {
        self.backend
            .thread_queue_update(ThreadQueueUpdateParams {
                thread_id: ThreadId(id),
                queued_submission_id,
                input,
            })
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn queue_delete(
        &self,
        id: String,
        queued_submission_id: QueuedSubmissionId,
    ) -> Result<ThreadQueueDeleteResponse, String> {
        self.backend
            .thread_queue_delete(ThreadQueueDeleteParams {
                thread_id: ThreadId(id),
                queued_submission_id,
            })
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn queue_reorder(
        &self,
        id: String,
        queued_submission_ids: Vec<QueuedSubmissionId>,
    ) -> Result<(), String> {
        self.backend
            .thread_queue_reorder(ThreadQueueReorderParams {
                thread_id: ThreadId(id),
                queued_submission_ids,
            })
            .await
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub async fn queue_start(
        &self,
        id: String,
        queued_submission_id: Option<QueuedSubmissionId>,
    ) -> Result<ThreadQueueStartResponse, String> {
        self.backend
            .thread_queue_start(ThreadQueueStartParams {
                thread_id: ThreadId(id),
                queued_submission_id,
            })
            .await
            .map_err(|e| e.to_string())
    }
    pub async fn thread_history(
        &self,
        id: String,
        before: Cursor,
    ) -> Result<ThreadReadResponse, String> {
        self.backend
            .thread_read(ThreadReadParams {
                thread_id: ThreadId(id),
                before: Some(before),
                limit: Some(20),
            })
            .await
            .map_err(|e| e.to_string())
    }
}
