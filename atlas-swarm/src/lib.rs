//! Eventually consistent membership for a small swarm of Iroh endpoints.

pub mod auth;
pub mod local;
mod log;
mod store;
mod topology;

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use ed25519_dalek::SigningKey;
use futures_util::{Sink, Stream};
use iroh::{
    endpoint::{presets, Connection, RecvStream, SendStream},
    Endpoint, EndpointAddr, SecretKey,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;

pub use log::{
    Commit, CommitId, MembershipOperation, MembershipView, NodeCoordinate, NodeRecord, PathAcl,
    PathEntry, PathOperation, PathResource, RepositoryRecord, ServicePath, ServiceRecord,
    SignedPathOperation, SignedUserMetadata, SwarmOperation, SwarmPath, SwarmView, UserId,
    UserMetadata, UserSignature, SECURITY_KEY_APPLICATION,
};
pub use store::{MemoryStore, Store, StoredIdentity};
pub use topology::neighbors;

pub const ALPN: &[u8] = b"atlas-swarm/1";
pub const SERVICE_ALPN: &[u8] = b"atlas-swarm/rpc/1";

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
        IrohTransport::new(send, recv),
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
}

impl Swarm {
    /// Starts a swarm node with a broker-configured root ACL.
    pub async fn start(
        node_name: impl Into<String>,
        root_acl: PathAcl,
        bootstrap: Option<EndpointAddr>,
        store: Arc<dyn Store>,
    ) -> Result<Self, SwarmError> {
        let swarm = Self::open(node_name.into(), root_acl, store, Uuid::new_v4()).await?;
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
        swarm_id: Uuid,
    ) -> Result<Self, SwarmError> {
        if node_name.is_empty() {
            return Err(SwarmError::EmptyNodeName);
        }
        let identity = match store.load_identity().await.map_err(SwarmError::Store)? {
            Some(identity) => identity,
            None => {
                let identity = StoredIdentity {
                    swarm_id,
                    secret_key: SecretKey::generate().to_bytes(),
                    node_name,
                    coordinate: NodeCoordinate {
                        x: rand::thread_rng().gen(),
                        y: rand::thread_rng().gen(),
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
            .alpns(vec![ALPN.to_vec(), SERVICE_ALPN.to_vec()])
            .bind()
            .await
            .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        if store.commits().await.map_err(SwarmError::Store)?.is_empty() {
            store
                .append_operation(
                    endpoint.id(),
                    MembershipOperation::Join(NodeRecord {
                        name: identity.node_name.clone(),
                        endpoint_id: endpoint.id(),
                        endpoint_addr: endpoint.addr(),
                        coordinate: identity.coordinate,
                    })
                    .into(),
                    endpoint.secret_key(),
                )
                .await
                .map_err(SwarmError::Store)?;
        }
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
        })
    }

    pub fn endpoint_id(&self) -> iroh::EndpointId {
        self.endpoint.id()
    }
    pub fn endpoint_addr(&self) -> EndpointAddr {
        self.endpoint.addr()
    }
    pub fn swarm_id(&self) -> Uuid {
        self.identity.swarm_id
    }
    pub fn node_name(&self) -> &str {
        &self.identity.node_name
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
        self.store
            .view(&*self.root_acl.read().await)
            .await
            .expect("store view failed")
    }

    pub async fn rename_node(&self, name: impl Into<String>) -> Result<(), SwarmError> {
        let name = name.into();
        if name.is_empty() {
            return Err(SwarmError::EmptyNodeName);
        }
        self.append_local(MembershipOperation::Rename { name })
            .await
    }

    pub async fn set_user_metadata(
        &self,
        key: &SigningKey,
        metadata: UserMetadata,
    ) -> Result<(), SwarmError> {
        self.append_local(SwarmOperation::UserMetadata(SignedUserMetadata::new(
            self.swarm_id(),
            metadata,
            key,
        )))
        .await
    }

    /// Accepts metadata signed by a local user without requiring the daemon to hold that
    /// user's private key.
    pub async fn submit_user_metadata(
        &self,
        metadata: SignedUserMetadata,
    ) -> Result<(), SwarmError> {
        if metadata.swarm_id != self.swarm_id() || !metadata.verify() {
            return Err(SwarmError::AuthenticationFailed);
        }
        self.append_local(SwarmOperation::UserMetadata(metadata))
            .await
    }

    pub async fn submit_path_operation(
        &self,
        operation: SignedPathOperation,
    ) -> Result<(), SwarmError> {
        if operation.swarm_id != self.swarm_id() || !operation.verify() {
            return Err(SwarmError::AuthenticationFailed);
        }
        let path = match &operation.operation {
            PathOperation::SetAcl { path, .. } => path.as_ref(),
            PathOperation::DefineService { path, .. }
            | PathOperation::DefineRepository { path, .. }
            | PathOperation::SetState { path, .. }
            | PathOperation::DeleteState { path }
            | PathOperation::RemoveResource { path } => Some(path),
        };
        let view = self.view().await;
        let allowed = match path {
            Some(path) => can_write(&view, path, operation.user),
            None => view
                .root_acl
                .as_ref()
                .is_some_and(|acl| acl.writers.contains(&operation.user)),
        };
        if !allowed {
            return Err(SwarmError::PathWriteDenied(
                path.map_or("/", SwarmPath::as_str).into(),
            ));
        }
        self.append_local(SwarmOperation::Path(operation)).await
    }

    pub async fn set_path_acl(
        &self,
        key: &SigningKey,
        path: SwarmPath,
        acl: PathAcl,
    ) -> Result<(), SwarmError> {
        self.append_path(
            key,
            PathOperation::SetAcl {
                path: Some(path),
                acl,
            },
        )
        .await
    }

    pub async fn set_root_acl(&self, key: &SigningKey, acl: PathAcl) -> Result<(), SwarmError> {
        let view = self.view().await;
        if !view
            .root_acl
            .as_ref()
            .is_some_and(|acl| acl.writers.contains(&UserId::from_signing_key(key)))
        {
            return Err(SwarmError::PathWriteDenied("/".into()));
        }
        self.append_local(SwarmOperation::Path(SignedPathOperation::new(
            self.swarm_id(),
            PathOperation::SetAcl { path: None, acl },
            key,
        )))
        .await
    }

    pub async fn advertise_service(
        &self,
        key: &SigningKey,
        path: SwarmPath,
        allowed_users: BTreeSet<UserId>,
    ) -> Result<(), SwarmError> {
        self.append_path(
            key,
            PathOperation::DefineService {
                path,
                service: ServiceRecord {
                    provider: self.endpoint.id(),
                    allowed_users,
                },
            },
        )
        .await
    }

    pub async fn define_repository(
        &self,
        key: &SigningKey,
        path: SwarmPath,
        endpoints: BTreeSet<iroh::EndpointId>,
        allowed_users: BTreeSet<UserId>,
    ) -> Result<(), SwarmError> {
        self.append_path(
            key,
            PathOperation::DefineRepository {
                path,
                repository: RepositoryRecord {
                    endpoints,
                    allowed_users,
                },
            },
        )
        .await
    }

    pub async fn set_state(
        &self,
        key: &SigningKey,
        path: SwarmPath,
        value: serde_json::Value,
    ) -> Result<(), SwarmError> {
        self.append_path(key, PathOperation::SetState { path, value })
            .await
    }

    pub async fn delete_state(&self, key: &SigningKey, path: SwarmPath) -> Result<(), SwarmError> {
        self.append_path(key, PathOperation::DeleteState { path })
            .await
    }

    pub async fn remove_service(
        &self,
        key: &SigningKey,
        path: SwarmPath,
    ) -> Result<(), SwarmError> {
        if !can_write(&self.view().await, &path, UserId::from_signing_key(key)) {
            return Err(SwarmError::PathWriteDenied(path.as_str().into()));
        }
        self.services.write().await.remove(&path);
        self.append_path(key, PathOperation::RemoveResource { path })
            .await
    }

    pub async fn remove_repository(
        &self,
        key: &SigningKey,
        path: SwarmPath,
    ) -> Result<(), SwarmError> {
        self.append_path(key, PathOperation::RemoveResource { path })
            .await
    }

    pub async fn serve<H, S>(
        &self,
        key: &SigningKey,
        path: SwarmPath,
        allowed_users: BTreeSet<UserId>,
        service: S,
    ) -> Result<(), SwarmError>
    where
        H: atlas_rpc::Service<S> + Send + Sync + 'static,
        S: Clone + Send + Sync + 'static,
    {
        self.services.write().await.insert(
            path.clone(),
            Arc::new(move |peer| H::register(service.clone(), &peer)),
        );
        self.advertise_service(key, path, allowed_users).await
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
        let node = view
            .membership
            .nodes
            .values()
            .find(|node| node.endpoint_id == service.provider)
            .ok_or_else(|| SwarmError::ServiceUnavailable(path.as_str().into()))?;
        let connection = self
            .endpoint
            .connect(node.endpoint_addr.clone(), SERVICE_ALPN)
            .await
            .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        authenticate_client(&connection, path, user_key).await?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        Ok(atlas_rpc::Peer::new(atlas_rpc::CborTransport(
            IrohTransport::new(send, recv),
        )))
    }

    async fn append_local(&self, operation: impl Into<SwarmOperation>) -> Result<(), SwarmError> {
        self.store
            .append_operation(
                self.endpoint.id(),
                operation.into(),
                self.endpoint.secret_key(),
            )
            .await
            .map_err(SwarmError::Store)?;
        let view = self.view().await;
        let _ = self.changes.send(view.membership.clone());
        let _ = self.view_changes.send(view);
        self.sync_known_nodes();
        Ok(())
    }

    async fn append_path(
        &self,
        key: &SigningKey,
        operation: PathOperation,
    ) -> Result<(), SwarmError> {
        let path = match &operation {
            PathOperation::SetAcl { path, .. } => path.as_ref(),
            PathOperation::DefineService { path, .. }
            | PathOperation::DefineRepository { path, .. }
            | PathOperation::SetState { path, .. }
            | PathOperation::DeleteState { path }
            | PathOperation::RemoveResource { path } => Some(path),
        };
        let view = self.view().await;
        if !path.is_some_and(|path| can_write(&view, path, UserId::from_signing_key(key))) {
            return Err(SwarmError::PathWriteDenied(
                path.map_or("/", SwarmPath::as_str).into(),
            ));
        }
        self.append_local(SwarmOperation::Path(SignedPathOperation::new(
            self.swarm_id(),
            operation,
            key,
        )))
        .await
    }

    fn start_listener(&self) {
        let endpoint = self.endpoint.clone();
        let store = self.store.clone();
        let root_acl = self.root_acl.clone();
        let changes = self.changes.clone();
        let view_changes = self.view_changes.clone();
        let services = self.services.clone();
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
                    tokio::spawn(async move {
                        let _ = accept_service(connection, services, store, root_acl).await;
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
            let nodes = match store.view(&*root_acl.read().await).await {
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
}

/// Returns the cumulative permissions granted by the root and every ancestor
/// of `path`. Child ACLs add permissions; they never revoke inherited access.
pub fn path_acl(view: &SwarmView, path: &SwarmPath) -> PathAcl {
    let mut acl = view.root_acl.clone().unwrap_or_default();
    let mut prefix = String::new();
    for segment in path.as_str().split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        if let Some(entry) = SwarmPath::new(prefix.clone()).and_then(|path| view.paths.get(&path)) {
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
    for segment in path.as_str().split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        if let Some(entry) = SwarmPath::new(prefix.clone()).and_then(|path| view.paths.get(&path)) {
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
    Ok(())
}

async fn merge_and_publish(
    remote: Vec<Commit>,
    store: Arc<dyn Store>,
    root_acl: Arc<RwLock<PathAcl>>,
    changes: broadcast::Sender<MembershipView>,
    view_changes: broadcast::Sender<SwarmView>,
) -> Result<(), SwarmError> {
    let changed = store.merge(remote).await.map_err(SwarmError::Store)?;
    if changed {
        let view = store
            .view(&*root_acl.read().await)
            .await
            .map_err(SwarmError::Store)?;
        let _ = changes.send(view.membership.clone());
        let _ = view_changes.send(view);
    }
    Ok(())
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

async fn accept_service(
    connection: Connection,
    services: Arc<RwLock<BTreeMap<SwarmPath, ServiceRegistrar>>>,
    store: Arc<dyn Store>,
    root_acl: Arc<RwLock<PathAcl>>,
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
    let view = store
        .view(&*root_acl.read().await)
        .await
        .map_err(SwarmError::Store)?;
    let allowed = can_access_service(&view, &hello.path, hello.user)
        && matches!(view.paths.get(&hello.path).and_then(|entry| entry.resource.as_ref()), Some(PathResource::Service(service)) if {
            service.allowed_users.contains(&hello.user) && proof.signature.verify(hello.user, &auth_bytes(&hello.path, &nonce))
        });
    write_frame(&mut send, &allowed).await?;
    send.finish()
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
        IrohTransport::new(send, recv),
    )));
    Ok(())
}

async fn write_frame<T: Serialize>(send: &mut SendStream, value: &T) -> Result<(), SwarmError> {
    let bytes = serde_cbor::to_vec(value).expect("frame serialization cannot fail");
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
    let mut bytes = vec![0; u32::from_be_bytes(length) as usize];
    recv.read_exact(&mut bytes)
        .await
        .map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    serde_cbor::from_slice(&bytes).map_err(|_| SwarmError::AuthenticationFailed)
}

struct IrohTransport {
    incoming: tokio::sync::mpsc::UnboundedReceiver<Result<bytes::Bytes, io::Error>>,
    outgoing: tokio::sync::mpsc::UnboundedSender<bytes::Bytes>,
}

impl IrohTransport {
    fn new(mut send: SendStream, mut recv: RecvStream) -> Self {
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
        Self { incoming, outgoing }
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
