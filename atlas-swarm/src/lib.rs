//! Eventually consistent membership for a small swarm of Iroh endpoints.

pub mod atlas_backend;
pub mod atlas_op_heads_store;
pub mod atlas_op_store;
pub mod auth;
mod binary;
pub mod local;
mod log;
pub mod native_jj;
mod redb_store;
pub mod repository;
mod secret;
mod store;
mod topology;
pub mod virtual_checkout;

use std::{
    collections::BTreeMap,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use ed25519_dalek::SigningKey;
use futures_util::{Sink, Stream};
use iroh::{
    Endpoint, EndpointAddr, SecretKey,
    endpoint::{Connection, RecvStream, SendStream, presets},
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{RwLock, broadcast};
use uuid::Uuid;

pub use binary::BinaryBlob;
pub use log::{
    Commit, CommitId, JJ_REPOSITORY_FORMAT_VERSION, MembershipOperation, MembershipView,
    NodeCoordinate, NodeRecord, PathAcl, PathEntry, PathOperation, PathResource, RepositoryId,
    RepositoryKind, RepositoryRecord, RepositorySnapshotId, SECURITY_KEY_APPLICATION, ServicePath,
    ServiceRecord, SwarmOperation, SwarmPath, SwarmView, UserId, UserMetadata, UserSignature,
};
pub use redb_store::RedbStore;
pub use secret::{EncryptedSecret, EncryptionPublicKey, secret_associated_data};
pub use store::{MemoryStore, Store, StoredIdentity};
pub use topology::neighbors;

pub const ALPN: &[u8] = b"atlas-swarm/1";
pub const SERVICE_ALPN: &[u8] = b"atlas-swarm/rpc/1";
pub const REPOSITORY_ALPN: &[u8] = b"atlas-swarm/repository/1";

/// Opens an authenticated direct Iroh connection to a service resolved from a swarm view.
/// Local callers should use `local::connect_local_service_with_agent` when a resolution
/// provides a local socket.
pub async fn connect_remote_service_with_agent(
    endpoint_addr: EndpointAddr,
    path: &SwarmPath,
    signer: &auth::UserSigner,
) -> Result<atlas_rpc::Peer, SwarmError> {
    let endpoint = Endpoint::builder(presets::N0)
        .alpns(vec![SERVICE_ALPN.to_vec()])
        .bind()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let connection = endpoint
        .connect(endpoint_addr, SERVICE_ALPN)
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    authenticate_client_with_agent(&connection, path, signer).await?;
    let (send, recv) = connection
        .open_bi()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    Ok(atlas_rpc::Peer::new(atlas_rpc::CborTransport(
        IrohTransport::new(send, recv, Some(endpoint)),
    )))
}

#[derive(Debug, Error)]
pub enum SwarmError {
    #[error("the node name must not be empty")]
    EmptyNodeName,
    #[error("store error: {0}")]
    Store(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("Iroh error: {0}")]
    Iroh(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("local socket error: {0}")]
    LocalIo(#[from] io::Error),
    #[error("service authentication failed")]
    AuthenticationFailed,
    #[error("service is unavailable: {0}")]
    ServiceUnavailable(String),
    #[error("path write access denied: {0}")]
    PathWriteDenied(String),
    #[error("no resource at path: {0}")]
    ResourceNotFound(String),
}

type ServiceRegistrar = Arc<dyn Fn(atlas_rpc::Peer) + Send + Sync>;

pub struct Swarm {
    endpoint: Endpoint,
    store: Arc<dyn Store>,
    identity: StoredIdentity,
    root_acl: Arc<RwLock<PathAcl>>,
    changes: broadcast::Sender<MembershipView>,
    view_changes: broadcast::Sender<SwarmView>,
    services: Arc<RwLock<BTreeMap<SwarmPath, ServiceRegistrar>>>,
    repositories: Option<repository::RepositoryDatabase>,
}

impl Swarm {
    /// Starts a swarm node with a broker-configured root ACL.
    pub async fn start(
        node_name: impl Into<String>,
        root_acl: PathAcl,
        bootstrap: Option<EndpointAddr>,
        store: Arc<dyn Store>,
    ) -> Result<Self, SwarmError> {
        Self::start_with_repository(node_name, root_acl, bootstrap, store, None).await
    }

    pub async fn start_with_repository(
        node_name: impl Into<String>,
        root_acl: PathAcl,
        bootstrap: Option<EndpointAddr>,
        store: Arc<dyn Store>,
        repositories: Option<repository::RepositoryDatabase>,
    ) -> Result<Self, SwarmError> {
        let swarm = Self::open(
            node_name.into(),
            root_acl,
            store,
            Uuid::new_v4(),
            repositories,
        )
        .await?;
        if let Some(bootstrap) = bootstrap {
            let connection = swarm
                .endpoint
                .connect(bootstrap, ALPN)
                .await
                .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
            swarm.sync_outbound(connection, true).await?;
        }
        swarm.start_listener();
        Ok(swarm)
    }

    async fn open(
        node_name: String,
        root_acl: PathAcl,
        store: Arc<dyn Store>,
        _swarm_id: Uuid,
        repositories: Option<repository::RepositoryDatabase>,
    ) -> Result<Self, SwarmError> {
        if node_name.is_empty() {
            return Err(SwarmError::EmptyNodeName);
        }
        let identity = match store.load_identity().await.map_err(SwarmError::Store)? {
            Some(identity) => identity,
            None => {
                let (encryption_secret_key, _) = secret::generate_encryption_keypair();
                let identity = StoredIdentity {
                    secret_key: SecretKey::generate().to_bytes(),
                    encryption_secret_key,
                    node_name,
                    coordinate: NodeCoordinate {
                        x: rand::thread_rng().r#gen(),
                        y: rand::thread_rng().r#gen(),
                    },
                };
                store
                    .save_identity(identity.clone())
                    .await
                    .map_err(SwarmError::Store)?;
                identity
            }
        };
        let key = SecretKey::from_bytes(&identity.secret_key);
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(key)
            .alpns(vec![
                ALPN.to_vec(),
                SERVICE_ALPN.to_vec(),
                REPOSITORY_ALPN.to_vec(),
            ])
            .bind()
            .await
            .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        let (changes, _) = broadcast::channel(64);
        let (view_changes, _) = broadcast::channel(64);
        Ok(Self {
            endpoint,
            store,
            identity,
            root_acl: Arc::new(RwLock::new(root_acl)),
            changes,
            view_changes,
            services: Arc::new(RwLock::new(BTreeMap::new())),
            repositories,
        })
    }

    pub fn endpoint_id(&self) -> iroh::EndpointId {
        self.endpoint.id()
    }
    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }
    pub async fn swarm_id(&self) -> Option<Uuid> {
        self.view().await.swarm_id
    }
    pub fn node_name(&self) -> &str {
        &self.identity.node_name
    }
    pub fn node_coordinate(&self) -> NodeCoordinate {
        self.identity.coordinate
    }
    pub fn encryption_public_key(&self) -> EncryptionPublicKey {
        secret::public_key_from_secret(&self.identity.encryption_secret_key)
    }
    pub fn store(&self) -> &Arc<dyn Store> {
        &self.store
    }
    pub fn subscribe(&self) -> broadcast::Receiver<MembershipView> {
        self.changes.subscribe()
    }
    pub fn subscribe_view(&self) -> broadcast::Receiver<SwarmView> {
        self.view_changes.subscribe()
    }
    pub async fn membership(&self) -> MembershipView {
        self.view().await.membership
    }
    pub async fn view(&self) -> SwarmView {
        self.store.view().await.expect("store view failed")
    }

    pub async fn submit_commit(&self, commit: Commit) -> Result<(), SwarmError> {
        if commit.author != self.endpoint.id() || !commit.verify_user() {
            return Err(SwarmError::AuthenticationFailed);
        }
        let view = self.view().await;
        match &commit.operation {
            SwarmOperation::Genesis { .. } if view.swarm_id.is_some() => {
                return Err(SwarmError::AuthenticationFailed);
            }
            SwarmOperation::Genesis { .. } => {}
            SwarmOperation::UserMetadata(_) => {}
            SwarmOperation::Membership(_) => {}
            SwarmOperation::Path(PathOperation::NodeJoin { node, .. })
                if node.endpoint_id != self.endpoint.id() =>
            {
                return Err(SwarmError::AuthenticationFailed);
            }
            SwarmOperation::Path(operation) => {
                if !path_operation_allowed(&view, operation, commit.author, commit.user) {
                    return Err(SwarmError::PathWriteDenied("operation path".into()));
                }
            }
            SwarmOperation::PathBatch(operations) => {
                if operations.is_empty()
                    || path_operations_overlap(operations)
                    || operations.iter().any(|operation| {
                        !path_operation_allowed(&view, operation, commit.author, commit.user)
                    })
                {
                    return Err(SwarmError::PathWriteDenied("path batch".into()));
                }
            }
        }
        self.store
            .append_commit(commit, self.endpoint.secret_key())
            .await
            .map_err(SwarmError::Store)?;
        let view = self.view().await;
        let _ = self.changes.send(view.membership.clone());
        let _ = self.view_changes.send(view);
        self.sync_known_nodes();
        Ok(())
    }

    pub async fn service(
        &self,
        path: &SwarmPath,
        user_key: &SigningKey,
    ) -> Result<atlas_rpc::Peer, SwarmError> {
        let view = self.view().await;
        let service = match view
            .paths
            .get(path)
            .and_then(|entry| entry.resource.as_ref())
        {
            Some(PathResource::Service(service)) => service,
            _ => return Err(SwarmError::ServiceUnavailable(path.as_str().into())),
        };
        let node = service.endpoint_addr.clone().unwrap_or_else(|| {
            view.membership
                .nodes
                .values()
                .find(|node| node.endpoint_id == service.provider)
                .map(|node| node.endpoint_addr.clone())
                .unwrap_or_else(|| EndpointAddr::new(service.provider))
        });
        let connection = self
            .endpoint
            .connect(node, SERVICE_ALPN)
            .await
            .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        authenticate_client(&connection, path, user_key).await?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        Ok(atlas_rpc::Peer::new(atlas_rpc::CborTransport(
            IrohTransport::new(send, recv, None),
        )))
    }

    /// Registers an RPC implementation served directly by this swarm endpoint.
    /// The caller is responsible for publishing a matching [`ServiceRecord`].
    pub async fn register_rpc_service<F>(&self, path: SwarmPath, register: F)
    where
        F: Fn(atlas_rpc::Peer) + Send + Sync + 'static,
    {
        self.services.write().await.insert(path, Arc::new(register));
    }

    pub async fn unregister_rpc_service(&self, path: &SwarmPath) {
        self.services.write().await.remove(path);
    }

    fn start_listener(&self) {
        let endpoint = self.endpoint.clone();
        let store = self.store.clone();
        let root_acl = self.root_acl.clone();
        let changes = self.changes.clone();
        let view_changes = self.view_changes.clone();
        let services = self.services.clone();
        let repositories = self.repositories.clone();
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let Ok(connection) = incoming.await else {
                    continue;
                };
                if connection.alpn() == ALPN {
                    let store = store.clone();
                    let changes = changes.clone();
                    let view_changes = view_changes.clone();
                    let root_acl = root_acl.clone();
                    tokio::spawn(async move {
                        let _ =
                            sync_inbound(connection, store, root_acl, changes, view_changes).await;
                    });
                } else if connection.alpn() == SERVICE_ALPN {
                    let services = services.clone();
                    let store = store.clone();
                    let root_acl = root_acl.clone();
                    let provider = endpoint.id();
                    tokio::spawn(async move {
                        let _ =
                            accept_service(connection, services, store, root_acl, provider).await;
                    });
                } else if connection.alpn() == REPOSITORY_ALPN {
                    let Some(repositories) = repositories.clone() else {
                        continue;
                    };
                    let store = store.clone();
                    let provider = endpoint.id();
                    tokio::spawn(async move {
                        let _ = accept_repository_replication(
                            connection,
                            store,
                            repositories,
                            provider,
                        )
                        .await;
                    });
                }
            }
        });
    }

    async fn sync_outbound(
        &self,
        connection: Connection,
        adopt_root_acl: bool,
    ) -> Result<(), SwarmError> {
        sync_outbound(
            connection,
            self.store.clone(),
            self.root_acl.clone(),
            self.changes.clone(),
            self.view_changes.clone(),
            adopt_root_acl,
        )
        .await
    }

    fn sync_known_nodes(&self) {
        let endpoint = self.endpoint.clone();
        let store = self.store.clone();
        let root_acl = self.root_acl.clone();
        let changes = self.changes.clone();
        let view_changes = self.view_changes.clone();
        tokio::spawn(async move {
            let nodes = match store.view().await {
                Ok(view) => view.membership.nodes,
                Err(_) => return,
            };
            for node in nodes
                .into_values()
                .filter(|node| node.endpoint_id != endpoint.id())
            {
                let Ok(connection) = endpoint.connect(node.endpoint_addr, ALPN).await else {
                    continue;
                };
                let _ = sync_outbound(
                    connection,
                    store.clone(),
                    root_acl.clone(),
                    changes.clone(),
                    view_changes.clone(),
                    false,
                )
                .await;
            }
        });
    }

    pub fn replicate_repository_job(&self, job: repository::ReplicationJob) {
        let Some(repositories) = self.repositories.clone() else {
            return;
        };
        let endpoint = self.endpoint.clone();
        let store = self.store.clone();
        tokio::spawn(async move {
            replicate_repository_job_once(endpoint, store, repositories, job).await;
        });
    }

    /// Retries durable replication jobs until every configured endpoint has
    /// acknowledged the snapshot. The jobs themselves live in redb, so daemon
    /// restarts do not lose work.
    pub fn start_repository_replication_worker(&self) {
        let Some(repositories) = self.repositories.clone() else {
            return;
        };
        let endpoint = self.endpoint.clone();
        let store = self.store.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                interval.tick().await;
                let Ok(jobs) = repositories.replication_jobs() else {
                    continue;
                };
                for job in jobs {
                    replicate_repository_job_once(
                        endpoint.clone(),
                        store.clone(),
                        repositories.clone(),
                        job,
                    )
                    .await;
                }
            }
        });
    }

    pub async fn fetch_repository_snapshot(
        &self,
        repository_id: RepositoryId,
        snapshot_id: &RepositorySnapshotId,
    ) -> Result<Option<repository::JujutsuSnapshot>, SwarmError> {
        let view = self.view().await;
        let Some(repository) = view.paths.values().find_map(|entry| match &entry.resource {
            Some(PathResource::Repository(repository)) if repository.id == repository_id => {
                Some(repository)
            }
            _ => None,
        }) else {
            return Err(SwarmError::ResourceNotFound(repository_id.to_string()));
        };
        for target in &repository.endpoints {
            if *target == self.endpoint.id() {
                continue;
            }
            let address = view
                .membership
                .nodes
                .values()
                .find(|node| node.endpoint_id == *target)
                .map(|node| node.endpoint_addr.clone())
                .unwrap_or_else(|| EndpointAddr::new(*target));
            let Ok(connection) = self.endpoint.connect(address, REPOSITORY_ALPN).await else {
                continue;
            };
            let (mut send, mut recv) = connection
                .open_bi()
                .await
                .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
            write_frame(
                &mut send,
                &RepositoryRequest::GetSnapshot {
                    repository_id,
                    snapshot_id: snapshot_id.clone(),
                },
            )
            .await?;
            send.finish()
                .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
            if let Some(snapshot) = read_frame(&mut recv).await? {
                return Ok(Some(snapshot));
            }
        }
        Ok(None)
    }

    pub async fn fetch_repository_object(
        &self,
        repository_id: RepositoryId,
        object: &repository::RepositoryObjectId,
    ) -> Result<Option<Vec<u8>>, SwarmError> {
        let view = self.view().await;
        let Some(repository) = view.paths.values().find_map(|entry| match &entry.resource {
            Some(PathResource::Repository(repository)) if repository.id == repository_id => {
                Some(repository)
            }
            _ => None,
        }) else {
            return Err(SwarmError::ResourceNotFound(repository_id.to_string()));
        };
        for target in &repository.endpoints {
            if *target == self.endpoint.id() {
                continue;
            }
            let address = view
                .membership
                .nodes
                .values()
                .find(|node| node.endpoint_id == *target)
                .map(|node| node.endpoint_addr.clone())
                .unwrap_or_else(|| EndpointAddr::new(*target));
            let Ok(connection) = self.endpoint.connect(address, REPOSITORY_ALPN).await else {
                continue;
            };
            let (mut send, mut recv) = connection
                .open_bi()
                .await
                .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
            write_frame(
                &mut send,
                &RepositoryRequest::GetObject {
                    repository_id,
                    object: object.clone(),
                },
            )
            .await?;
            send.finish()
                .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
            let length: Option<u64> = read_frame(&mut recv).await?;
            let Some(length) = length else { continue };
            if length > MAX_REPOSITORY_OBJECT_BYTES {
                return Err(SwarmError::AuthenticationFailed);
            }
            let mut bytes = vec![0; length as usize];
            recv.read_exact(&mut bytes)
                .await
                .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
            return Ok(Some(bytes));
        }
        Ok(None)
    }
}

async fn replicate_repository_job_once(
    endpoint: Endpoint,
    store: Arc<dyn Store>,
    repositories: repository::RepositoryDatabase,
    job: repository::ReplicationJob,
) {
    let Ok(view) = store.view().await else {
        return;
    };
    for target in job.pending_endpoints.clone() {
        let address = view
            .membership
            .nodes
            .values()
            .find(|node| node.endpoint_id == target)
            .map(|node| node.endpoint_addr.clone())
            .unwrap_or_else(|| EndpointAddr::new(target));
        let Ok(connection) = endpoint.connect(address, REPOSITORY_ALPN).await else {
            continue;
        };
        if send_repository_replication(connection, &repositories, job.repository_id, &job.snapshot)
            .await
            .is_ok()
        {
            let _ = repositories.complete_replication_endpoint(
                job.repository_id,
                &job.snapshot,
                target,
            );
        }
    }
}

/// Serves a standalone Iroh endpoint whose authorization follows a watched swarm service path.
pub async fn serve_remote_registered<F>(
    endpoint: Endpoint,
    path: SwarmPath,
    state: Arc<RwLock<local::PathState>>,
    register: F,
) -> Result<(), SwarmError>
where
    F: Fn(&atlas_rpc::Peer) + Clone + Send + Sync + 'static,
{
    let provider = endpoint.id();
    while let Some(incoming) = endpoint.accept().await {
        let path = path.clone();
        let state = state.clone();
        let register = register.clone();
        tokio::spawn(async move {
            let Ok(connection) = incoming.await else {
                return;
            };
            let Ok(peer) =
                accept_standalone_service(connection, &path, &*state.read().await, provider).await
            else {
                return;
            };
            register(&peer);
            peer.closed().await;
        });
    }
    Ok(())
}

fn path_of(operation: &PathOperation) -> Option<&SwarmPath> {
    match operation {
        PathOperation::SetAcl { path, .. }
        | PathOperation::NodeJoin { path, .. }
        | PathOperation::DefineService { path, .. }
        | PathOperation::DefineRepository { path, .. }
        | PathOperation::PublishRepositorySnapshot { path, .. }
        | PathOperation::SetConfig { path, .. }
        | PathOperation::Remove { path } => Some(path),
        PathOperation::NodeMove { from, .. } => Some(from),
    }
}

fn path_operation_allowed(
    view: &SwarmView,
    operation: &PathOperation,
    author: iroh::EndpointId,
    user: UserId,
) -> bool {
    let semantically_valid = match operation {
        PathOperation::NodeJoin { node, .. } => node.endpoint_id == author,
        PathOperation::NodeMove { node, from, to } => {
            from != to
                && matches!(
                    view.paths.get(from).and_then(|entry| entry.resource.as_ref()),
                    Some(PathResource::Node(record)) if record.endpoint_id == *node
                )
        }
        PathOperation::SetAcl { path, acl } if path.as_str() == "/" => !acl.writers.is_empty(),
        _ => true,
    };
    semantically_valid
        && match operation {
            PathOperation::NodeMove { from, to, .. } => {
                can_write(view, from, user) && can_write(view, to, user)
            }
            _ => path_of(operation).is_some_and(|path| can_write(view, path, user)),
        }
}

fn path_operations_overlap(operations: &[PathOperation]) -> bool {
    let mut paths = std::collections::BTreeSet::new();
    operations.iter().any(|operation| match operation {
        PathOperation::NodeMove { from, to, .. } => {
            !paths.insert(from.as_str()) || !paths.insert(to.as_str())
        }
        operation => path_of(operation).is_none_or(|path| !paths.insert(path.as_str())),
    })
}

/// Returns the cumulative permissions granted by the root and every ancestor
/// of `path`. Child ACLs add permissions; they never revoke inherited access.
pub fn path_acl(view: &SwarmView, path: &SwarmPath) -> PathAcl {
    let mut acl = view.root_acl.clone().unwrap_or_default();
    let mut prefix = String::new();
    for segment in path
        .as_str()
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
    {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        if let Some(entry) =
            SwarmPath::new(format!("/{prefix}")).and_then(|path| view.paths.get(&path))
        {
            if let Some(entry_acl) = &entry.acl {
                acl.readers.extend(entry_acl.readers.iter().copied());
                acl.writers.extend(entry_acl.writers.iter().copied());
            }
        }
    }
    acl
}

/// Whether `user` has write access to `path`, including permissions inherited
/// from all ancestors.
pub fn can_write(view: &SwarmView, path: &SwarmPath, user: UserId) -> bool {
    path_acl(view, path).writers.contains(&user)
}

/// Whether `user` has read access to `path`, including permissions inherited
/// from all ancestors.
pub fn can_read(view: &SwarmView, path: &SwarmPath, user: UserId) -> bool {
    path_acl(view, path).readers.contains(&user)
}

fn service_acl<'a>(view: &'a SwarmView, path: &SwarmPath) -> Option<&'a PathAcl> {
    let mut acl = view.root_acl.as_ref();
    let mut prefix = String::new();
    for segment in path
        .as_str()
        .trim_start_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
    {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        if let Some(entry) =
            SwarmPath::new(format!("/{prefix}")).and_then(|path| view.paths.get(&path))
        {
            if entry.acl.is_some() {
                acl = entry.acl.as_ref();
            }
        }
    }
    acl
}

fn can_access_service(view: &SwarmView, path: &SwarmPath, user: UserId) -> bool {
    service_acl(view, path).is_some_and(|acl| acl.readers.contains(&user))
}

const MAX_LOG_BYTES: usize = 16 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
struct SwarmSync {
    root_acl: PathAcl,
    commits: Vec<Commit>,
}

async fn sync_outbound(
    connection: Connection,
    store: Arc<dyn Store>,
    root_acl: Arc<RwLock<PathAcl>>,
    changes: broadcast::Sender<MembershipView>,
    view_changes: broadcast::Sender<SwarmView>,
    adopt_root_acl: bool,
) -> Result<(), SwarmError> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let bytes = serde_cbor::to_vec(&SwarmSync {
        root_acl: root_acl.read().await.clone(),
        commits: store.commits().await.map_err(SwarmError::Store)?,
    })
    .expect("commit serialization cannot fail");
    send.write_all(&bytes)
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    send.finish()
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let remote = recv
        .read_to_end(MAX_LOG_BYTES)
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let remote: SwarmSync =
        serde_cbor::from_slice(&remote).map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    if adopt_root_acl {
        *root_acl.write().await = remote.root_acl.clone();
    }
    merge_and_publish(remote.commits, store, root_acl, changes, view_changes).await
}

async fn sync_inbound(
    connection: Connection,
    store: Arc<dyn Store>,
    root_acl: Arc<RwLock<PathAcl>>,
    changes: broadcast::Sender<MembershipView>,
    view_changes: broadcast::Sender<SwarmView>,
) -> Result<(), SwarmError> {
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let remote = recv
        .read_to_end(MAX_LOG_BYTES)
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let remote: SwarmSync =
        serde_cbor::from_slice(&remote).map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    merge_and_publish(
        remote.commits,
        store.clone(),
        root_acl.clone(),
        changes.clone(),
        view_changes.clone(),
    )
    .await?;
    let bytes = serde_cbor::to_vec(&SwarmSync {
        root_acl: root_acl.read().await.clone(),
        commits: store.commits().await.map_err(SwarmError::Store)?,
    })
    .expect("commit serialization cannot fail");
    send.write_all(&bytes)
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    send.finish()
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    send.stopped()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    Ok(())
}

async fn merge_and_publish(
    remote: Vec<Commit>,
    store: Arc<dyn Store>,
    _root_acl: Arc<RwLock<PathAcl>>,
    changes: broadcast::Sender<MembershipView>,
    view_changes: broadcast::Sender<SwarmView>,
) -> Result<(), SwarmError> {
    let changed = store.merge(remote).await.map_err(SwarmError::Store)?;
    if changed {
        let view = store.view().await.map_err(SwarmError::Store)?;
        let _ = changes.send(view.membership.clone());
        let _ = view_changes.send(view);
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct RepositoryReplicationHeader {
    repository_id: RepositoryId,
    snapshot_id: RepositorySnapshotId,
    snapshot: repository::JujutsuSnapshot,
}

#[derive(Deserialize, Serialize)]
enum RepositoryRequest {
    Replicate(RepositoryReplicationHeader),
    GetSnapshot {
        repository_id: RepositoryId,
        snapshot_id: RepositorySnapshotId,
    },
    GetObject {
        repository_id: RepositoryId,
        object: repository::RepositoryObjectId,
    },
}

#[derive(Deserialize, Serialize)]
struct RepositoryObjectHeader {
    object: repository::RepositoryObjectId,
    length: u64,
}

const MAX_REPOSITORY_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;

async fn send_repository_replication(
    connection: Connection,
    repositories: &repository::RepositoryDatabase,
    repository_id: RepositoryId,
    snapshot_id: &RepositorySnapshotId,
) -> Result<(), SwarmError> {
    let snapshot = repositories
        .read_snapshot(repository_id, snapshot_id)
        .map_err(SwarmError::Store)?
        .ok_or_else(|| SwarmError::ResourceNotFound(snapshot_id.to_string()))?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    write_frame(
        &mut send,
        &RepositoryRequest::Replicate(RepositoryReplicationHeader {
            repository_id,
            snapshot_id: snapshot_id.clone(),
            snapshot,
        }),
    )
    .await?;
    let missing: Vec<repository::RepositoryObjectId> = read_frame(&mut recv).await?;
    for object in missing {
        let bytes = repositories
            .get_object_by_id(repository_id, &object)
            .map_err(SwarmError::Store)?
            .ok_or_else(|| SwarmError::ResourceNotFound(object.id.len().to_string()))?;
        write_frame(
            &mut send,
            &RepositoryObjectHeader {
                object,
                length: bytes.len() as u64,
            },
        )
        .await?;
        send.write_all(&bytes)
            .await
            .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    }
    send.finish()
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let accepted: bool = read_frame(&mut recv).await?;
    accepted
        .then_some(())
        .ok_or(SwarmError::AuthenticationFailed)
}

async fn accept_repository_replication(
    connection: Connection,
    store: Arc<dyn Store>,
    repositories: repository::RepositoryDatabase,
    provider: iroh::EndpointId,
) -> Result<(), SwarmError> {
    let remote = connection.remote_id();
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let request: RepositoryRequest = read_frame(&mut recv).await?;
    let view = store.view().await.map_err(SwarmError::Store)?;
    let repository_id = match &request {
        RepositoryRequest::Replicate(header) => header.repository_id,
        RepositoryRequest::GetSnapshot { repository_id, .. }
        | RepositoryRequest::GetObject { repository_id, .. } => *repository_id,
    };
    let repository = view.paths.values().find_map(|entry| match &entry.resource {
        Some(PathResource::Repository(repository)) if repository.id == repository_id => {
            Some(repository)
        }
        _ => None,
    });
    let authorized = repository.is_some_and(|repository| {
        repository.endpoints.contains(&provider)
            && (repository.endpoints.contains(&remote)
                || view
                    .membership
                    .nodes
                    .values()
                    .any(|node| node.endpoint_id == remote))
    });
    if !authorized {
        return Err(SwarmError::AuthenticationFailed);
    }
    let RepositoryRequest::Replicate(header) = request else {
        match request {
            RepositoryRequest::GetSnapshot {
                repository_id,
                snapshot_id,
            } => {
                let snapshot = repositories
                    .read_snapshot(repository_id, &snapshot_id)
                    .map_err(SwarmError::Store)?;
                write_frame(&mut send, &snapshot).await?;
            }
            RepositoryRequest::GetObject {
                repository_id,
                object,
            } => {
                let bytes = repositories
                    .get_object_by_id(repository_id, &object)
                    .map_err(SwarmError::Store)?;
                write_frame(&mut send, &bytes.as_ref().map(|bytes| bytes.len() as u64)).await?;
                if let Some(bytes) = bytes {
                    send.write_all(&bytes)
                        .await
                        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
                }
            }
            RepositoryRequest::Replicate(_) => unreachable!(),
        }
        send.finish()
            .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        send.stopped()
            .await
            .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        return Ok(());
    };
    if header.snapshot.validate().is_err() {
        return Err(SwarmError::AuthenticationFailed);
    }
    let mut missing = Vec::new();
    for object in &header.snapshot.objects {
        if !repositories
            .has_object(header.repository_id, object)
            .map_err(SwarmError::Store)?
        {
            missing.push(object.clone());
        }
    }
    write_frame(&mut send, &missing).await?;
    for expected in missing {
        let object: RepositoryObjectHeader = read_frame(&mut recv).await?;
        if object.object != expected || object.length > MAX_REPOSITORY_OBJECT_BYTES {
            return Err(SwarmError::AuthenticationFailed);
        }
        let mut bytes = vec![0; object.length as usize];
        recv.read_exact(&mut bytes)
            .await
            .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        repositories
            .put_object_with_id(
                header.repository_id,
                object.object.kind,
                &object.object.id,
                &bytes,
            )
            .map_err(SwarmError::Store)?;
    }
    let stored_id = repositories
        .write_snapshot(header.repository_id, &header.snapshot)
        .map_err(SwarmError::Store)?;
    let accepted = stored_id == header.snapshot_id;
    write_frame(&mut send, &accepted).await?;
    send.finish()
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    send.stopped()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    accepted
        .then_some(())
        .ok_or(SwarmError::AuthenticationFailed)
}

#[derive(Deserialize, Serialize)]
struct ServiceHello {
    path: SwarmPath,
    user: UserId,
}
#[derive(Deserialize, Serialize)]
struct ServiceChallenge {
    nonce: [u8; 32],
}
#[derive(Deserialize, Serialize)]
struct ServiceProof {
    signature: UserSignature,
}

pub(crate) fn auth_bytes(path: &SwarmPath, nonce: &[u8; 32]) -> Vec<u8> {
    serde_cbor::to_vec(&(b"atlas-swarm/service-auth/1", path, nonce))
        .expect("authentication serialization cannot fail")
}

async fn authenticate_client(
    connection: &Connection,
    path: &SwarmPath,
    key: &SigningKey,
) -> Result<(), SwarmError> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    write_frame(
        &mut send,
        &ServiceHello {
            path: path.clone(),
            user: UserId::from_signing_key(key),
        },
    )
    .await?;
    let challenge: ServiceChallenge = read_frame(&mut recv).await?;
    let signature = UserSignature::Ed25519(
        ed25519_dalek::Signer::sign(key, &auth_bytes(path, &challenge.nonce))
            .to_bytes()
            .to_vec(),
    );
    write_frame(&mut send, &ServiceProof { signature }).await?;
    send.finish()
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let accepted: bool = read_frame(&mut recv).await?;
    accepted
        .then_some(())
        .ok_or(SwarmError::AuthenticationFailed)
}

async fn authenticate_client_with_agent(
    connection: &Connection,
    path: &SwarmPath,
    signer: &auth::UserSigner,
) -> Result<(), SwarmError> {
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    write_frame(
        &mut send,
        &ServiceHello {
            path: path.clone(),
            user: signer.user(),
        },
    )
    .await?;
    let challenge: ServiceChallenge = read_frame(&mut recv).await?;
    let signature = signer.sign(&auth_bytes(path, &challenge.nonce)).await?;
    write_frame(&mut send, &ServiceProof { signature }).await?;
    send.finish()
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let accepted: bool = read_frame(&mut recv).await?;
    accepted
        .then_some(())
        .ok_or(SwarmError::AuthenticationFailed)
}

async fn accept_standalone_service(
    connection: Connection,
    expected_path: &SwarmPath,
    state: &local::PathState,
    provider: iroh::EndpointId,
) -> Result<atlas_rpc::Peer, SwarmError> {
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let hello: ServiceHello = read_frame(&mut recv).await?;
    let mut nonce = [0; 32];
    rand::thread_rng().fill(&mut nonce);
    write_frame(&mut send, &ServiceChallenge { nonce }).await?;
    let proof: ServiceProof = read_frame(&mut recv).await?;
    let allowed = hello.path == *expected_path
        && state.path == *expected_path
        && state
            .effective_acl
            .as_ref()
            .is_some_and(|acl| acl.readers.contains(&hello.user))
        && matches!(
            state.entry.as_ref().and_then(|entry| entry.resource.as_ref()),
            Some(PathResource::Service(service))
                if service.provider == provider
                    && service.allowed_users.contains(&hello.user)
                    && proof.signature.verify(
                        hello.user,
                        &auth_bytes(&hello.path, &nonce),
                    )
        );
    write_frame(&mut send, &allowed).await?;
    send.finish()
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    send.stopped()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    if !allowed {
        return Err(SwarmError::AuthenticationFailed);
    }
    let (send, recv) = connection
        .accept_bi()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    Ok(atlas_rpc::Peer::new(atlas_rpc::CborTransport(
        IrohTransport::new(send, recv, None),
    )))
}

async fn accept_service(
    connection: Connection,
    services: Arc<RwLock<BTreeMap<SwarmPath, ServiceRegistrar>>>,
    store: Arc<dyn Store>,
    _root_acl: Arc<RwLock<PathAcl>>,
    provider: iroh::EndpointId,
) -> Result<(), SwarmError> {
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let hello: ServiceHello = read_frame(&mut recv).await?;
    let mut nonce = [0; 32];
    rand::thread_rng().fill(&mut nonce);
    write_frame(&mut send, &ServiceChallenge { nonce }).await?;
    let proof: ServiceProof = read_frame(&mut recv).await?;
    let view = store.view().await.map_err(SwarmError::Store)?;
    let allowed = can_access_service(&view, &hello.path, hello.user)
        && matches!(view.paths.get(&hello.path).and_then(|entry| entry.resource.as_ref()), Some(PathResource::Service(service)) if {
            service.provider == provider
                && service.allowed_users.contains(&hello.user)
                && proof.signature.verify(hello.user, &auth_bytes(&hello.path, &nonce))
        });
    write_frame(&mut send, &allowed).await?;
    send.finish()
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    send.stopped()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    if !allowed {
        return Err(SwarmError::AuthenticationFailed);
    }
    let registrar = services
        .read()
        .await
        .get(&hello.path)
        .cloned()
        .ok_or_else(|| SwarmError::ServiceUnavailable(hello.path.as_str().into()))?;
    let (send, recv) = connection
        .accept_bi()
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    registrar(atlas_rpc::Peer::new(atlas_rpc::CborTransport(
        IrohTransport::new(send, recv, None),
    )));
    Ok(())
}

const MAX_CONTROL_FRAME_BYTES: usize = 64 * 1024 * 1024;

async fn write_frame<T: Serialize>(send: &mut SendStream, value: &T) -> Result<(), SwarmError> {
    let bytes = serde_cbor::to_vec(value).expect("frame serialization cannot fail");
    if bytes.len() > MAX_CONTROL_FRAME_BYTES {
        return Err(SwarmError::AuthenticationFailed);
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| SwarmError::AuthenticationFailed)?
        .to_be_bytes();
    send.write_all(&length)
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    send.write_all(&bytes)
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))
}

async fn read_frame<T: for<'de> Deserialize<'de>>(recv: &mut RecvStream) -> Result<T, SwarmError> {
    let mut length = [0; 4];
    recv.read_exact(&mut length)
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_CONTROL_FRAME_BYTES {
        return Err(SwarmError::AuthenticationFailed);
    }
    let mut bytes = vec![0; length];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    serde_cbor::from_slice(&bytes).map_err(|_| SwarmError::AuthenticationFailed)
}

struct IrohTransport {
    incoming: tokio::sync::mpsc::UnboundedReceiver<Result<bytes::Bytes, io::Error>>,
    outgoing: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>,
    _endpoint: Option<Endpoint>,
}

impl IrohTransport {
    fn new(mut send: SendStream, mut recv: RecvStream, endpoint: Option<Endpoint>) -> Self {
        let (outgoing, mut writes) = tokio::sync::mpsc::unbounded_channel::<bytes::Bytes>();
        let (reads, incoming) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some(bytes) = writes.recv().await {
                let length = (bytes.len() as u32).to_be_bytes();
                if send.write_all(&length).await.is_err() || send.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            let _ = send.finish();
        });
        tokio::spawn(async move {
            loop {
                let mut length = [0; 4];
                if recv.read_exact(&mut length).await.is_err() {
                    break;
                }
                let mut bytes = vec![0; u32::from_be_bytes(length) as usize];
                if recv.read_exact(&mut bytes).await.is_err() {
                    break;
                }
                if reads.send(Ok(bytes::Bytes::from(bytes))).is_err() {
                    break;
                }
            }
        });
        Self {
            incoming,
            outgoing,
            _endpoint: endpoint,
        }
    }
}

impl Stream for IrohTransport {
    type Item = Result<bytes::Bytes, io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.incoming.poll_recv(cx)
    }
}

impl Sink<bytes::Bytes> for IrohTransport {
    type Error = io::Error;
    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn start_send(self: Pin<&mut Self>, item: bytes::Bytes) -> Result<(), Self::Error> {
        self.outgoing
            .send(item)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "Iroh stream closed"))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_rpc::interface;
    use std::collections::BTreeSet;

    #[interface]
    trait Echo {
        async fn echo(&self, request: String) -> Result<String, String>;
    }

    struct EchoService;

    impl Echo for EchoService {
        async fn echo(&self, request: String) -> Result<String, String> {
            Ok(request)
        }
    }

    #[tokio::test]
    async fn standalone_service_endpoint_authenticates_and_serves_rpc() {
        let service_key = SecretKey::generate();
        let service_endpoint = Endpoint::builder(presets::N0)
            .secret_key(service_key.clone())
            .alpns(vec![SERVICE_ALPN.to_vec()])
            .bind()
            .await
            .unwrap();
        let service_addr = service_endpoint.addr();
        let path = SwarmPath::new("/acp/test").unwrap();
        let signing_key = SigningKey::from_bytes(&[42; 32]);
        let user = UserId::from_signing_key(&signing_key);
        let state = local::PathState {
            path: path.clone(),
            entry: Some(PathEntry {
                resource: Some(PathResource::Service(ServiceRecord {
                    provider: service_key.public(),
                    endpoint_addr: Some(service_addr.clone()),
                    allowed_users: [user].into_iter().collect(),
                })),
                ..Default::default()
            }),
            effective_acl: Some(PathAcl {
                readers: [user].into_iter().collect(),
                writers: BTreeSet::new(),
            }),
        };
        let server = tokio::spawn(serve_remote_registered(
            service_endpoint,
            path.clone(),
            Arc::new(RwLock::new(state)),
            |peer| peer.register::<EchoHandle, _>(EchoService),
        ));

        let peer = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            connect_remote_service_with_agent(
                service_addr,
                &path,
                &auth::UserSigner::File(signing_key),
            ),
        )
        .await
        .expect("standalone service connection timed out")
        .unwrap();
        assert_eq!(
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                EchoHandle::new(peer).echo("hello".to_owned()),
            )
            .await
            .expect("standalone RPC timed out")
            .unwrap(),
            "hello"
        );
        server.abort();
    }
}
