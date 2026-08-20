//! Unix-socket control and service helpers for a local swarm daemon.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use atlas_rpc::{CborTransport, Peer, interface};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::StreamExt;
use futures_util::{Sink, Stream};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{RwLock, watch},
};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use uuid::Uuid;

use crate::{
    Commit, CommitId, PathOperation, PathResource, Swarm, SwarmError, SwarmOperation, SwarmPath,
    SwarmView, UserId, UserSignature, auth_bytes,
    repository::{
        CheckoutId, CheckoutObjectKind, JujutsuSnapshot, RepositoryDatabase, RepositoryObjectId,
    },
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum StateSelector {
    RootAcl,
    Membership,
    Users,
    User { user: UserId },
    Path { path: SwarmPath },
    Paths { prefix: Option<SwarmPath> },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PathState {
    pub path: SwarmPath,
    pub entry: Option<crate::PathEntry>,
    pub effective_acl: Option<crate::PathAcl>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum StateSnapshot {
    RootAcl(Option<crate::PathAcl>),
    Membership(crate::MembershipView),
    Users(BTreeMap<UserId, crate::UserMetadata>),
    User {
        user: UserId,
        metadata: Option<crate::UserMetadata>,
    },
    Path(Box<PathState>),
    Paths(BTreeMap<SwarmPath, crate::PathEntry>),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StateChange {
    pub revision: u64,
    pub snapshot: StateSnapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterLocalService {
    pub commit: Commit,
    pub socket: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolveServiceRequest {
    pub path: SwarmPath,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceResolution {
    pub provider: iroh::EndpointId,
    pub endpoint_addr: iroh::EndpointAddr,
    pub local_socket: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SwarmInfo {
    pub swarm_id: Option<Uuid>,
    pub root_acl: crate::PathAcl,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitHistoryRequest {
    pub starts: Vec<CommitId>,
    pub depth: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitHistory {
    pub commits: Vec<Commit>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryObjectRequest {
    pub repository_id: crate::RepositoryId,
    pub user: crate::UserId,
    pub object: RepositoryObjectId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PutRepositoryObjectRequest {
    pub repository_id: crate::RepositoryId,
    pub user: crate::UserId,
    pub object: RepositoryObjectId,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckoutObjectRequest {
    pub repository_id: crate::RepositoryId,
    pub checkout_id: CheckoutId,
    pub user: crate::UserId,
    pub kind: CheckoutObjectKind,
    pub id: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PutCheckoutObjectRequest {
    pub repository_id: crate::RepositoryId,
    pub checkout_id: CheckoutId,
    pub user: crate::UserId,
    pub kind: CheckoutObjectKind,
    pub id: Vec<u8>,
    pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckoutObjectIdsRequest {
    pub repository_id: crate::RepositoryId,
    pub checkout_id: CheckoutId,
    pub user: crate::UserId,
    pub kind: CheckoutObjectKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckoutOpHeadsRequest {
    pub repository_id: crate::RepositoryId,
    pub checkout_id: CheckoutId,
    pub user: crate::UserId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateCheckoutOpHeadsRequest {
    pub repository_id: crate::RepositoryId,
    pub checkout_id: CheckoutId,
    pub user: crate::UserId,
    pub old_ids: Vec<Vec<u8>>,
    pub new_id: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositorySnapshotRequest {
    pub repository_id: crate::RepositoryId,
    pub user: crate::UserId,
    pub snapshot_id: crate::RepositorySnapshotId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PutRepositorySnapshotRequest {
    pub repository_id: crate::RepositoryId,
    pub user: crate::UserId,
    pub snapshot: JujutsuSnapshot,
}

#[interface]
pub trait SwarmControl {
    async fn shutdown(&self, request: ()) -> Result<(), String>;
    async fn endpoint_id(&self, request: ()) -> Result<iroh::EndpointId, String>;
    async fn info(&self, request: ()) -> Result<SwarmInfo, String>;
    async fn submit_commit(&self, commit: Commit) -> Result<(), String>;
    async fn commit_history(&self, request: CommitHistoryRequest) -> Result<CommitHistory, String>;
    async fn register_local_service(&self, request: RegisterLocalService) -> Result<(), String>;
    async fn unregister_local_service(&self, request: ResolveServiceRequest) -> Result<(), String>;
    async fn resolve_service(
        &self,
        request: ResolveServiceRequest,
    ) -> Result<ServiceResolution, String>;
    async fn query(&self, selector: StateSelector) -> Result<StateSnapshot, String>;
    #[rpc(reply_and_stream)]
    async fn watch(
        &self,
        selector: StateSelector,
    ) -> Result<(StateSnapshot, atlas_rpc::Stream<StateChange>), String>;
    async fn get_config(&self, path: SwarmPath) -> Result<Option<serde_json::Value>, String>;
    async fn set_config(&self, commit: Commit) -> Result<(), String>;
    #[rpc(reply_and_stream)]
    async fn watch_config(
        &self,
        path: SwarmPath,
    ) -> Result<
        (
            Option<serde_json::Value>,
            atlas_rpc::Stream<Option<serde_json::Value>>,
        ),
        String,
    >;
    async fn path_state(&self, path: SwarmPath) -> Result<PathState, String>;
    async fn get_repository_object(
        &self,
        request: RepositoryObjectRequest,
    ) -> Result<Option<Vec<u8>>, String>;
    async fn put_repository_object(
        &self,
        request: PutRepositoryObjectRequest,
    ) -> Result<(), String>;
    async fn get_checkout_object(
        &self,
        request: CheckoutObjectRequest,
    ) -> Result<Option<Vec<u8>>, String>;
    async fn put_checkout_object(&self, request: PutCheckoutObjectRequest) -> Result<(), String>;
    async fn checkout_object_ids(
        &self,
        request: CheckoutObjectIdsRequest,
    ) -> Result<Vec<Vec<u8>>, String>;
    async fn checkout_op_heads(
        &self,
        request: CheckoutOpHeadsRequest,
    ) -> Result<Vec<Vec<u8>>, String>;
    async fn update_checkout_op_heads(
        &self,
        request: UpdateCheckoutOpHeadsRequest,
    ) -> Result<(), String>;
    async fn get_repository_snapshot(
        &self,
        request: RepositorySnapshotRequest,
    ) -> Result<Option<JujutsuSnapshot>, String>;
    async fn put_repository_snapshot(
        &self,
        request: PutRepositorySnapshotRequest,
    ) -> Result<crate::RepositorySnapshotId, String>;
    async fn publish_repository_snapshot(&self, commit: Commit) -> Result<(), String>;
}

#[derive(Clone)]
pub struct LocalDaemon {
    swarm: Arc<Swarm>,
    repositories: RepositoryDatabase,
    services: Arc<RwLock<BTreeMap<SwarmPath, LocalService>>>,
    shutdown: watch::Sender<bool>,
    connection: Option<Uuid>,
}

#[derive(Clone)]
struct LocalService {
    socket: PathBuf,
    connection: Option<Uuid>,
}

impl LocalDaemon {
    pub fn new(swarm: Arc<Swarm>, repositories: RepositoryDatabase) -> Self {
        let (shutdown, _) = watch::channel(false);
        Self {
            swarm,
            repositories,
            services: Arc::new(RwLock::new(BTreeMap::new())),
            shutdown,
            connection: None,
        }
    }

    fn for_connection(&self) -> Self {
        Self {
            swarm: self.swarm.clone(),
            repositories: self.repositories.clone(),
            services: self.services.clone(),
            shutdown: self.shutdown.clone(),
            connection: Some(Uuid::new_v4()),
        }
    }

    async fn remove_connection_services(&self) {
        let Some(connection) = self.connection else {
            return;
        };
        self.services
            .write()
            .await
            .retain(|_, service| service.connection != Some(connection));
    }
}

impl SwarmControl for LocalDaemon {
    async fn shutdown(&self, _: ()) -> Result<(), String> {
        self.shutdown.send_replace(true);
        Ok(())
    }

    async fn endpoint_id(&self, _: ()) -> Result<iroh::EndpointId, String> {
        Ok(self.swarm.endpoint_id())
    }
    async fn info(&self, _: ()) -> Result<SwarmInfo, String> {
        let view = self.swarm.view().await;
        Ok(SwarmInfo {
            swarm_id: self.swarm.swarm_id().await,
            root_acl: view.root_acl.unwrap_or_default(),
        })
    }
    async fn submit_commit(&self, commit: Commit) -> Result<(), String> {
        self.swarm
            .submit_commit(commit)
            .await
            .map_err(|error| error.to_string())
    }

    async fn commit_history(&self, request: CommitHistoryRequest) -> Result<CommitHistory, String> {
        let commits = self
            .swarm
            .store()
            .commits()
            .await
            .map_err(|error| error.to_string())?;
        let by_id: BTreeMap<_, _> = commits
            .into_iter()
            .map(|commit| (commit.id, commit))
            .collect();
        let starts = if request.starts.is_empty() {
            by_id
                .values()
                .filter(|commit| {
                    !by_id
                        .values()
                        .any(|other| other.parents.contains(&commit.id))
                })
                .map(|commit| commit.id)
                .collect()
        } else {
            if request.starts.iter().any(|id| !by_id.contains_key(id)) {
                return Err("unknown commit start".into());
            }
            request.starts
        };
        let mut pending: Vec<_> = starts.into_iter().map(|id| (id, 0)).collect();
        let mut selected = BTreeMap::new();
        let mut truncated = false;
        while let Some((id, depth)) = pending.pop() {
            if selected.contains_key(&id) {
                continue;
            }
            let commit = by_id[&id].clone();
            if depth == request.depth {
                truncated |= !commit.parents.is_empty();
            } else {
                pending.extend(
                    commit
                        .parents
                        .iter()
                        .copied()
                        .map(|parent| (parent, depth + 1)),
                );
            }
            selected.insert(id, commit);
        }
        Ok(CommitHistory {
            commits: selected.into_values().collect(),
            truncated,
        })
    }

    async fn register_local_service(&self, request: RegisterLocalService) -> Result<(), String> {
        let SwarmOperation::Path(PathOperation::DefineService { path, service }) =
            &request.commit.operation
        else {
            return Err("local registration requires a service definition".into());
        };
        if service.provider != self.swarm.endpoint_id() {
            return Err("local service provider must be this daemon endpoint".into());
        }
        if !request.socket.is_absolute() {
            return Err("local service socket must be absolute".into());
        }
        let path = path.clone();
        self.swarm
            .submit_commit(request.commit)
            .await
            .map_err(|error| error.to_string())?;
        self.services.write().await.insert(
            path,
            LocalService {
                socket: request.socket,
                connection: self.connection,
            },
        );
        Ok(())
    }

    async fn unregister_local_service(&self, request: ResolveServiceRequest) -> Result<(), String> {
        self.services.write().await.remove(&request.path);
        Ok(())
    }

    async fn resolve_service(
        &self,
        request: ResolveServiceRequest,
    ) -> Result<ServiceResolution, String> {
        let view = self.swarm.view().await;
        let service = match view
            .paths
            .get(&request.path)
            .and_then(|entry| entry.resource.as_ref())
        {
            Some(PathResource::Service(service)) => service,
            _ => return Err(format!("service is unavailable: {}", request.path.as_str())),
        };
        let endpoint_addr = service.endpoint_addr.clone().unwrap_or_else(|| {
            view.membership
                .nodes
                .values()
                .find(|node| node.endpoint_id == service.provider)
                .map(|node| node.endpoint_addr.clone())
                .unwrap_or_else(|| iroh::EndpointAddr::new(service.provider))
        });
        let local_socket = if service.provider == self.swarm.endpoint_id() {
            self.services
                .read()
                .await
                .get(&request.path)
                .map(|service| service.socket.clone())
        } else {
            None
        };
        Ok(ServiceResolution {
            provider: service.provider,
            endpoint_addr,
            local_socket,
        })
    }

    async fn query(&self, selector: StateSelector) -> Result<StateSnapshot, String> {
        Ok(select_state(&self.swarm.view().await, selector))
    }

    async fn watch(
        &self,
        selector: StateSelector,
    ) -> Result<(StateSnapshot, atlas_rpc::Stream<StateChange>), String> {
        let snapshot = select_state(&self.swarm.view().await, selector.clone());
        let updates = tokio_stream::wrappers::BroadcastStream::new(self.swarm.subscribe_view())
            .filter_map(move |view| {
                let selector = selector.clone();
                async move {
                    view.ok().map(|view| StateChange {
                        revision: 0,
                        snapshot: select_state(&view, selector),
                    })
                }
            });
        Ok((snapshot, atlas_rpc::Stream::new(updates)))
    }

    async fn path_state(&self, path: SwarmPath) -> Result<PathState, String> {
        match self.query(StateSelector::Path { path }).await? {
            StateSnapshot::Path(state) => Ok(*state),
            _ => unreachable!(),
        }
    }

    async fn get_config(&self, path: SwarmPath) -> Result<Option<serde_json::Value>, String> {
        Ok(
            match self
                .swarm
                .view()
                .await
                .paths
                .get(&path)
                .and_then(|entry| entry.resource.as_ref())
            {
                Some(PathResource::Config(value)) => Some(value.clone()),
                _ => None,
            },
        )
    }
    async fn set_config(&self, commit: Commit) -> Result<(), String> {
        if !matches!(
            commit.operation,
            SwarmOperation::Path(PathOperation::SetConfig { .. })
        ) {
            return Err("set_config requires a SetConfig operation".into());
        }
        self.swarm
            .submit_commit(commit)
            .await
            .map_err(|error| error.to_string())
    }
    async fn watch_config(
        &self,
        path: SwarmPath,
    ) -> Result<
        (
            Option<serde_json::Value>,
            atlas_rpc::Stream<Option<serde_json::Value>>,
        ),
        String,
    > {
        let initial = self.get_config(path.clone()).await?;
        let updates = tokio_stream::wrappers::BroadcastStream::new(self.swarm.subscribe_view())
            .filter_map(move |view| {
                let path = path.clone();
                async move {
                    view.ok().map(|view| {
                        match view
                            .paths
                            .get(&path)
                            .and_then(|entry| entry.resource.as_ref())
                        {
                            Some(PathResource::Config(value)) => Some(value.clone()),
                            _ => None,
                        }
                    })
                }
            });
        Ok((initial, atlas_rpc::Stream::new(updates)))
    }

    async fn get_repository_object(
        &self,
        request: RepositoryObjectRequest,
    ) -> Result<Option<Vec<u8>>, String> {
        self.authorize_repository(request.repository_id, request.user, false)
            .await?;
        if let Some(bytes) = self
            .repositories
            .get_object_by_id(request.repository_id, &request.object)
            .map_err(|error| error.to_string())?
        {
            return Ok(Some(bytes));
        }
        let bytes = self
            .swarm
            .fetch_repository_object(request.repository_id, &request.object)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(bytes) = &bytes {
            self.repositories
                .put_object_by_id(request.repository_id, &request.object, bytes)
                .map_err(|error| error.to_string())?;
        }
        Ok(bytes)
    }

    async fn put_repository_object(
        &self,
        request: PutRepositoryObjectRequest,
    ) -> Result<(), String> {
        self.authorize_repository(request.repository_id, request.user, true)
            .await?;
        if request.object.id.is_empty() {
            return Err("repository object id is empty".into());
        }
        self.repositories
            .put_object_by_id(request.repository_id, &request.object, &request.bytes)
            .map_err(|error| error.to_string())
    }

    async fn get_checkout_object(
        &self,
        request: CheckoutObjectRequest,
    ) -> Result<Option<Vec<u8>>, String> {
        self.authorize_repository(request.repository_id, request.user, false)
            .await?;
        self.repositories
            .get_checkout_object(
                request.repository_id,
                request.checkout_id,
                request.kind,
                &request.id,
            )
            .map_err(|error| error.to_string())
    }

    async fn put_checkout_object(&self, request: PutCheckoutObjectRequest) -> Result<(), String> {
        self.authorize_repository(request.repository_id, request.user, true)
            .await?;
        if request.id.is_empty() {
            return Err("checkout object id is empty".into());
        }
        self.repositories
            .put_checkout_object(
                request.repository_id,
                request.checkout_id,
                request.kind,
                &request.id,
                &request.bytes,
            )
            .map_err(|error| error.to_string())
    }

    async fn checkout_object_ids(
        &self,
        request: CheckoutObjectIdsRequest,
    ) -> Result<Vec<Vec<u8>>, String> {
        self.authorize_repository(request.repository_id, request.user, false)
            .await?;
        self.repositories
            .checkout_object_ids(request.repository_id, request.checkout_id, request.kind)
            .map_err(|error| error.to_string())
    }

    async fn checkout_op_heads(
        &self,
        request: CheckoutOpHeadsRequest,
    ) -> Result<Vec<Vec<u8>>, String> {
        self.authorize_repository(request.repository_id, request.user, false)
            .await?;
        self.repositories
            .checkout_op_heads(request.repository_id, request.checkout_id)
            .map_err(|error| error.to_string())
    }

    async fn update_checkout_op_heads(
        &self,
        request: UpdateCheckoutOpHeadsRequest,
    ) -> Result<(), String> {
        self.authorize_repository(request.repository_id, request.user, true)
            .await?;
        if request.new_id.is_empty() {
            return Err("operation head id is empty".into());
        }
        self.repositories
            .update_checkout_op_heads(
                request.repository_id,
                request.checkout_id,
                &request.old_ids,
                &request.new_id,
            )
            .map_err(|error| error.to_string())
    }

    async fn get_repository_snapshot(
        &self,
        request: RepositorySnapshotRequest,
    ) -> Result<Option<JujutsuSnapshot>, String> {
        self.authorize_repository(request.repository_id, request.user, false)
            .await?;
        if let Some(snapshot) = self
            .repositories
            .read_snapshot(request.repository_id, &request.snapshot_id)
            .map_err(|error| error.to_string())?
        {
            return Ok(Some(snapshot));
        }
        let snapshot = self
            .swarm
            .fetch_repository_snapshot(request.repository_id, &request.snapshot_id)
            .await
            .map_err(|error| error.to_string())?;
        if let Some(snapshot) = &snapshot {
            let stored = self
                .repositories
                .write_snapshot(request.repository_id, snapshot)
                .map_err(|error| error.to_string())?;
            if stored != request.snapshot_id {
                return Err("remote repository snapshot hash mismatch".into());
            }
        }
        Ok(snapshot)
    }

    async fn put_repository_snapshot(
        &self,
        request: PutRepositorySnapshotRequest,
    ) -> Result<crate::RepositorySnapshotId, String> {
        self.authorize_repository(request.repository_id, request.user, true)
            .await?;
        request.snapshot.validate().map_err(str::to_owned)?;
        for object in &request.snapshot.objects {
            if !self
                .repositories
                .has_object(request.repository_id, object)
                .map_err(|error| error.to_string())?
            {
                return Err(format!(
                    "snapshot references missing {:?} object",
                    object.kind
                ));
            }
        }
        self.repositories
            .write_snapshot(request.repository_id, &request.snapshot)
            .map_err(|error| error.to_string())
    }

    async fn publish_repository_snapshot(&self, commit: Commit) -> Result<(), String> {
        let SwarmOperation::Path(PathOperation::PublishRepositorySnapshot {
            repository_id,
            snapshot,
            ..
        }) = &commit.operation
        else {
            return Err("repository publication requires PublishRepositorySnapshot".into());
        };
        let repository_id = *repository_id;
        let snapshot = snapshot.clone();
        if self
            .repositories
            .read_snapshot(repository_id, &snapshot)
            .map_err(|error| error.to_string())?
            .is_none()
        {
            return Err("repository snapshot is not stored locally".into());
        }
        self.swarm
            .submit_commit(commit)
            .await
            .map_err(|error| error.to_string())?;
        let endpoint = self.swarm.endpoint_id();
        let view = self.swarm.view().await;
        let Some(repository) = view.paths.values().find_map(|entry| match &entry.resource {
            Some(PathResource::Repository(repository)) if repository.id == repository_id => {
                Some(repository)
            }
            _ => None,
        }) else {
            return Err("published repository disappeared from the resolved view".into());
        };
        let mut pending_endpoints = repository.endpoints.clone();
        pending_endpoints.remove(&endpoint);
        if !pending_endpoints.is_empty() {
            let job = crate::repository::ReplicationJob {
                repository_id,
                snapshot,
                pending_endpoints,
            };
            self.repositories
                .enqueue_replication(&job)
                .map_err(|error| error.to_string())?;
            self.swarm.replicate_repository_job(job);
        }
        Ok(())
    }
}

impl LocalDaemon {
    async fn authorize_repository(
        &self,
        repository_id: crate::RepositoryId,
        user: crate::UserId,
        write: bool,
    ) -> Result<(), String> {
        let view = self.swarm.view().await;
        let Some((path, repository)) =
            view.paths
                .iter()
                .find_map(|(path, entry)| match entry.resource.as_ref() {
                    Some(PathResource::Repository(repository))
                        if repository.id == repository_id =>
                    {
                        Some((path, repository))
                    }
                    _ => None,
                })
        else {
            return Err("repository does not exist in this swarm".into());
        };
        if !repository.allowed_users.contains(&user)
            || if write {
                !crate::can_write(&view, path, user)
            } else {
                !crate::can_read(&view, path, user)
            }
        {
            return Err("repository access denied".into());
        }
        Ok(())
    }
}

fn select_state(view: &SwarmView, selector: StateSelector) -> StateSnapshot {
    match selector {
        StateSelector::RootAcl => StateSnapshot::RootAcl(view.root_acl.clone()),
        StateSelector::Membership => StateSnapshot::Membership(view.membership.clone()),
        StateSelector::Users => StateSnapshot::Users(view.users.clone()),
        StateSelector::User { user } => StateSnapshot::User {
            user,
            metadata: view.users.get(&user).cloned(),
        },
        StateSelector::Path { path } => StateSnapshot::Path(Box::new(path_state(view, path))),
        StateSelector::Paths { prefix } => StateSnapshot::Paths(
            view.paths
                .iter()
                .filter(|(path, _)| {
                    prefix.as_ref().is_none_or(|prefix| {
                        path.as_str() == prefix.as_str()
                            || path.as_str().starts_with(&format!("{}/", prefix.as_str()))
                    })
                })
                .map(|(path, entry)| (path.clone(), entry.clone()))
                .collect(),
        ),
    }
}

fn path_state(view: &SwarmView, path: SwarmPath) -> PathState {
    PathState {
        entry: view.paths.get(&path).cloned(),
        effective_acl: crate::service_acl(view, &path).cloned(),
        path,
    }
}

pub fn default_socket() -> io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    Ok(runtime.join("atlas").join("swarm.sock"))
}

pub async fn serve_daemon(socket: &Path, daemon: LocalDaemon) -> io::Result<()> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        match UnixStream::connect(socket).await {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "atlas-swarm daemon already owns the socket",
                ));
            }
            Err(_) => std::fs::remove_file(socket)?,
        }
    }
    let listener = UnixListener::bind(socket)?;
    #[cfg(unix)]
    {
        std::fs::set_permissions(socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    }
    let mut shutdown = daemon.shutdown.subscribe();
    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, _) = result?;
                let daemon = daemon.for_connection();
                tokio::spawn(async move {
                    let peer = unix_peer(stream);
                    peer.register::<SwarmControlHandle, _>(daemon.clone());
                    peer.closed().await;
                    daemon.remove_connection_services().await;
                });
            }
            result = shutdown.changed() => {
                result.map_err(|_| io::Error::other("atlas-swarm shutdown channel closed"))?;
                break;
            }
        }
    }
    drop(listener);
    if let Err(error) = std::fs::remove_file(socket)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(error);
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    Ok(())
}

pub async fn connect_control(socket: &Path) -> io::Result<SwarmControlHandle> {
    let stream = UnixStream::connect(socket).await?;
    Ok(SwarmControlHandle::new(unix_peer(stream)))
}

pub async fn autostart() -> io::Result<SwarmControlHandle> {
    let socket = default_socket()?;
    autostart_at(&socket, false).await
}

pub async fn autostart_at(socket: &Path, reset: bool) -> io::Result<SwarmControlHandle> {
    autostart_at_mode(socket, reset, None).await
}

pub async fn autostart_with_executable(executable: &Path) -> io::Result<SwarmControlHandle> {
    let socket = default_socket()?;
    autostart_at_mode(&socket, false, Some(executable)).await
}

pub async fn autostart_at_with_executable(
    socket: &Path,
    reset: bool,
    executable: &Path,
) -> io::Result<SwarmControlHandle> {
    autostart_at_mode(socket, reset, Some(executable)).await
}

async fn autostart_at_mode(
    socket: &Path,
    reset: bool,
    executable: Option<&Path>,
) -> io::Result<SwarmControlHandle> {
    if reset {
        reset_daemon(socket).await?;
        return start_daemon(socket, executable).await;
    }
    match connect_control(socket).await {
        Ok(control) => Ok(control),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            start_daemon(socket, executable).await
        }
        Err(error) => Err(error),
    }
}

pub async fn reset_daemon(socket: &Path) -> io::Result<()> {
    match connect_control(socket).await {
        Ok(control) => {
            control.shutdown(()).await.map_err(io::Error::other)?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
            loop {
                match UnixStream::connect(socket).await {
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                        ) =>
                    {
                        return Ok(());
                    }
                    Ok(_) if tokio::time::Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(50)).await
                    }
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "atlas-swarm daemon did not shut down within 3 seconds",
                        ));
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

async fn start_daemon(
    socket: &Path,
    daemon_executable: Option<&Path>,
) -> io::Result<SwarmControlHandle> {
    let signer = crate::auth::UserSigner::discover().await?;
    let mut command = if let Some(executable) = daemon_executable {
        std::process::Command::new(executable)
    } else {
        let executable = std::env::current_exe()?;
        let candidate = executable.parent().map(|parent| parent.join("atlas-swarm"));
        if candidate.as_ref().is_some_and(|path| path.exists()) {
            std::process::Command::new(candidate.unwrap())
        } else {
            std::process::Command::new("atlas-swarm")
        }
    };
    command
        .arg("serve")
        .arg("--root-user")
        .arg(signer.user().to_string())
        .arg("--socket")
        .arg(socket)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match connect_control(socket).await {
            Ok(control) => return Ok(control),
            Err(error)
                if tokio::time::Instant::now() < deadline
                    && matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await
            }
            Err(error) => return Err(error),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct ServiceHello {
    path: SwarmPath,
    user: UserId,
}
#[derive(Serialize, Deserialize)]
struct ServiceChallenge {
    nonce: [u8; 32],
}
#[derive(Serialize, Deserialize)]
struct ServiceProof {
    signature: UserSignature,
}

pub async fn connect_local_service(
    socket: &Path,
    path: &SwarmPath,
    key: &SigningKey,
) -> Result<Peer, SwarmError> {
    let mut stream = UnixStream::connect(socket).await?;
    write_frame(
        &mut stream,
        &ServiceHello {
            path: path.clone(),
            user: UserId::from_signing_key(key),
        },
    )
    .await?;
    let challenge: ServiceChallenge = read_frame(&mut stream).await?;
    let signature = UserSignature::Ed25519(
        key.sign(&auth_bytes(path, &challenge.nonce))
            .to_bytes()
            .to_vec(),
    );
    write_frame(&mut stream, &ServiceProof { signature }).await?;
    let accepted: bool = read_frame(&mut stream).await?;
    if !accepted {
        return Err(SwarmError::AuthenticationFailed);
    }
    Ok(unix_peer(stream))
}

/// Connects with an `ssh-ed25519` identity held by the local SSH agent.
pub async fn connect_local_service_with_agent(
    socket: &Path,
    path: &SwarmPath,
    signer: &crate::auth::UserSigner,
) -> Result<Peer, SwarmError> {
    let mut stream = UnixStream::connect(socket).await?;
    write_frame(
        &mut stream,
        &ServiceHello {
            path: path.clone(),
            user: signer.user(),
        },
    )
    .await?;
    let challenge: ServiceChallenge = read_frame(&mut stream).await?;
    let signature = signer.sign(&auth_bytes(path, &challenge.nonce)).await?;
    write_frame(&mut stream, &ServiceProof { signature }).await?;
    let accepted: bool = read_frame(&mut stream).await?;
    if !accepted {
        return Err(SwarmError::AuthenticationFailed);
    }
    Ok(unix_peer(stream))
}

pub async fn accept_local_service(
    stream: &mut UnixStream,
    state: &PathState,
) -> Result<SwarmPath, SwarmError> {
    let hello: ServiceHello = read_frame(stream).await?;
    let mut nonce = [0; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
    write_frame(stream, &ServiceChallenge { nonce }).await?;
    let proof: ServiceProof = read_frame(stream).await?;
    let allowed = hello.path == state.path
        && state
            .effective_acl
            .as_ref()
            .is_some_and(|acl| acl.readers.contains(&hello.user))
        && matches!(state.entry.as_ref().and_then(|entry| entry.resource.as_ref()), Some(PathResource::Service(service)) if service.allowed_users.contains(&hello.user) && proof.signature.verify(hello.user, &auth_bytes(&hello.path, &nonce)));
    write_frame(stream, &allowed).await?;
    if !allowed {
        return Err(SwarmError::AuthenticationFailed);
    }
    Ok(hello.path)
}

pub async fn serve_local<H, S>(
    listener: UnixListener,
    path: SwarmPath,
    view: Arc<RwLock<PathState>>,
    service: S,
) -> io::Result<()>
where
    H: atlas_rpc::Service<S> + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    loop {
        let (mut stream, _) = listener.accept().await?;
        let view = view.clone();
        let path = path.clone();
        let service = service.clone();
        tokio::spawn(async move {
            if matches!(accept_local_service(&mut stream, &*view.read().await).await, Ok(accepted_path) if accepted_path == path)
            {
                let peer = unix_peer(stream);
                H::register(service, &peer);
                peer.closed().await;
            }
        });
    }
}

/// Serves a local swarm service whose RPC registration needs multiple interfaces.
pub async fn serve_local_registered<F>(
    listener: UnixListener,
    path: SwarmPath,
    view: Arc<RwLock<PathState>>,
    register: F,
) -> io::Result<()>
where
    F: Fn(&Peer) + Clone + Send + Sync + 'static,
{
    loop {
        let (mut stream, _) = listener.accept().await?;
        let view = view.clone();
        let path = path.clone();
        let register = register.clone();
        tokio::spawn(async move {
            if matches!(accept_local_service(&mut stream, &*view.read().await).await, Ok(accepted_path) if accepted_path == path)
            {
                let peer = unix_peer(stream);
                register(&peer);
                peer.closed().await;
            }
        });
    }
}

fn unix_peer(stream: UnixStream) -> Peer {
    let codec = LengthDelimitedCodec::builder()
        .max_frame_length(1024 * 1024 * 1024)
        .new_codec();
    Peer::new(CborTransport(UnixTransport(Framed::new(stream, codec))))
}

struct UnixTransport(Framed<UnixStream, LengthDelimitedCodec>);

impl Stream for UnixTransport {
    type Item = Result<bytes::Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.0)
            .poll_next(cx)
            .map(|item| item.map(|item| item.map(|bytes| bytes.freeze())))
    }
}

impl Sink<bytes::Bytes> for UnixTransport {
    type Error = io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0).poll_ready(cx)
    }
    fn start_send(mut self: Pin<&mut Self>, item: bytes::Bytes) -> Result<(), Self::Error> {
        Pin::new(&mut self.0).start_send(item)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0).poll_close(cx)
    }
}

async fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<(), SwarmError> {
    let bytes = serde_cbor::to_vec(value).map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    stream
        .write_all(
            &u32::try_from(bytes.len())
                .map_err(|_| SwarmError::AuthenticationFailed)?
                .to_be_bytes(),
        )
        .await?;
    stream.write_all(&bytes).await?;
    Ok(())
}

async fn read_frame<T: for<'de> Deserialize<'de>>(
    stream: &mut UnixStream,
) -> Result<T, SwarmError> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).await?;
    let mut bytes = vec![0; u32::from_be_bytes(length) as usize];
    stream.read_exact(&mut bytes).await?;
    serde_cbor::from_slice(&bytes).map_err(|_| SwarmError::AuthenticationFailed)
}
