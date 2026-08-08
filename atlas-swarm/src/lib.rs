//! Eventually consistent membership for a small swarm of Iroh endpoints.

mod log;
mod store;
mod topology;

use std::{collections::{BTreeMap, BTreeSet}, io, pin::Pin, sync::Arc, task::{Context, Poll}};

use ed25519_dalek::SigningKey;
use futures_util::{Sink, Stream};
use iroh::{endpoint::{presets, Connection, RecvStream, SendStream}, Endpoint, EndpointAddr, SecretKey};
use rand::Rng;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;

pub use log::{Commit, CommitId, MembershipOperation, MembershipView, NodeCoordinate, NodeRecord, ServicePath, ServiceRecord, SignedUserMetadata, SwarmOperation, SwarmView, UserId, UserMetadata};
pub use store::{MemoryStore, Store, StoredIdentity};
pub use topology::neighbors;

pub const ALPN: &[u8] = b"atlas-swarm/1";
pub const SERVICE_ALPN: &[u8] = b"atlas-swarm/rpc/1";

#[derive(Debug, Error)]
pub enum SwarmError {
    #[error("the node name must not be empty")]
    EmptyNodeName,
    #[error("store error: {0}")]
    Store(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("Iroh error: {0}")]
    Iroh(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("service authentication failed")]
    AuthenticationFailed,
    #[error("service is unavailable: {0}")]
    ServiceUnavailable(String),
}

type ServiceRegistrar = Arc<dyn Fn(atlas_rpc::Peer) + Send + Sync>;

pub struct Swarm {
    endpoint: Endpoint,
    store: Arc<dyn Store>,
    identity: StoredIdentity,
    changes: broadcast::Sender<MembershipView>,
    view_changes: broadcast::Sender<SwarmView>,
    services: Arc<RwLock<BTreeMap<ServicePath, ServiceRegistrar>>>,
}

impl Swarm {
    pub async fn create(node_name: impl Into<String>, store: Arc<dyn Store>) -> Result<Self, SwarmError> {
        let swarm = Self::open(node_name.into(), store, Uuid::new_v4()).await?;
        swarm.start_listener();
        Ok(swarm)
    }

    pub async fn join(node_name: impl Into<String>, bootstrap: EndpointAddr, store: Arc<dyn Store>) -> Result<Self, SwarmError> {
        let swarm = Self::open(node_name.into(), store, Uuid::new_v4()).await?;
        let connection = swarm.endpoint.connect(bootstrap, ALPN).await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        swarm.sync_outbound(connection).await?;
        swarm.start_listener();
        Ok(swarm)
    }

    async fn open(node_name: String, store: Arc<dyn Store>, swarm_id: Uuid) -> Result<Self, SwarmError> {
        if node_name.is_empty() { return Err(SwarmError::EmptyNodeName); }
        let identity = match store.load_identity().await.map_err(SwarmError::Store)? {
            Some(identity) => identity,
            None => {
                let identity = StoredIdentity { swarm_id, secret_key: SecretKey::generate().to_bytes(), node_name, coordinate: NodeCoordinate { x: rand::thread_rng().gen(), y: rand::thread_rng().gen() } };
                store.save_identity(identity.clone()).await.map_err(SwarmError::Store)?;
                identity
            }
        };
        let key = SecretKey::from_bytes(&identity.secret_key);
        let endpoint = Endpoint::builder(presets::N0).secret_key(key).alpns(vec![ALPN.to_vec(), SERVICE_ALPN.to_vec()]).bind().await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        if store.commits().await.map_err(SwarmError::Store)?.is_empty() {
            store.append_operation(endpoint.id(), MembershipOperation::Join(NodeRecord { name: identity.node_name.clone(), endpoint_id: endpoint.id(), endpoint_addr: endpoint.addr(), coordinate: identity.coordinate }).into(), endpoint.secret_key()).await.map_err(SwarmError::Store)?;
        }
        let (changes, _) = broadcast::channel(64);
        let (view_changes, _) = broadcast::channel(64);
        Ok(Self { endpoint, store, identity, changes, view_changes, services: Arc::new(RwLock::new(BTreeMap::new())) })
    }

    pub fn endpoint_id(&self) -> iroh::EndpointId { self.endpoint.id() }
    pub fn endpoint_addr(&self) -> EndpointAddr { self.endpoint.addr() }
    pub fn swarm_id(&self) -> Uuid { self.identity.swarm_id }
    pub fn node_name(&self) -> &str { &self.identity.node_name }
    pub fn store(&self) -> &Arc<dyn Store> { &self.store }
    pub fn subscribe(&self) -> broadcast::Receiver<MembershipView> { self.changes.subscribe() }
    pub fn subscribe_view(&self) -> broadcast::Receiver<SwarmView> { self.view_changes.subscribe() }
    pub async fn membership(&self) -> MembershipView { self.view().await.membership }
    pub async fn view(&self) -> SwarmView { self.store.view().await.expect("store view failed") }

    pub async fn rename_node(&self, name: impl Into<String>) -> Result<(), SwarmError> {
        let name = name.into();
        if name.is_empty() { return Err(SwarmError::EmptyNodeName); }
        self.append_local(MembershipOperation::Rename { name }).await
    }

    pub async fn set_user_metadata(&self, key: &SigningKey, metadata: UserMetadata) -> Result<(), SwarmError> {
        self.append_local(SwarmOperation::UserMetadata(SignedUserMetadata::new(metadata, key))).await
    }

    pub async fn advertise_service(&self, path: ServicePath, allowed_users: BTreeSet<UserId>) -> Result<(), SwarmError> {
        self.append_local(SwarmOperation::AdvertiseService(ServiceRecord { path, provider: self.endpoint.id(), allowed_users })).await
    }

    pub async fn remove_service(&self, path: ServicePath) -> Result<(), SwarmError> {
        self.services.write().await.remove(&path);
        self.append_local(SwarmOperation::RemoveService { path, provider: self.endpoint.id() }).await
    }

    pub async fn serve<H, S>(&self, path: ServicePath, allowed_users: BTreeSet<UserId>, service: S) -> Result<(), SwarmError>
    where
        H: atlas_rpc::Service<S> + Send + Sync + 'static,
        S: Clone + Send + Sync + 'static,
    {
        self.services.write().await.insert(path.clone(), Arc::new(move |peer| H::register(service.clone(), &peer)));
        self.advertise_service(path, allowed_users).await
    }

    pub async fn service(&self, path: &ServicePath, user_key: &SigningKey) -> Result<atlas_rpc::Peer, SwarmError> {
        let view = self.view().await;
        let service = view.services.get(path).ok_or_else(|| SwarmError::ServiceUnavailable(path.as_str().into()))?;
        let node = view.membership.nodes.values().find(|node| node.endpoint_id == service.provider).ok_or_else(|| SwarmError::ServiceUnavailable(path.as_str().into()))?;
        let connection = self.endpoint.connect(node.endpoint_addr.clone(), SERVICE_ALPN).await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        authenticate_client(&connection, path, user_key).await?;
        let (send, recv) = connection.open_bi().await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        Ok(atlas_rpc::Peer::new(atlas_rpc::CborTransport(IrohTransport::new(send, recv))))
    }

    async fn append_local(&self, operation: impl Into<SwarmOperation>) -> Result<(), SwarmError> {
        self.store.append_operation(self.endpoint.id(), operation.into(), self.endpoint.secret_key()).await.map_err(SwarmError::Store)?;
        let view = self.store.view().await.map_err(SwarmError::Store)?;
        let _ = self.changes.send(view.membership.clone());
        let _ = self.view_changes.send(view);
        self.sync_known_nodes();
        Ok(())
    }

    fn start_listener(&self) {
        let endpoint = self.endpoint.clone();
        let store = self.store.clone();
        let changes = self.changes.clone();
        let view_changes = self.view_changes.clone();
        let services = self.services.clone();
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let Ok(connection) = incoming.await else { continue; };
                if connection.alpn() == ALPN {
                    let store = store.clone();
                    let changes = changes.clone();
                    let view_changes = view_changes.clone();
                    tokio::spawn(async move { let _ = sync_inbound(connection, store, changes, view_changes).await; });
                } else if connection.alpn() == SERVICE_ALPN {
                    let services = services.clone();
                    let store = store.clone();
                    tokio::spawn(async move { let _ = accept_service(connection, services, store).await; });
                }
            }
        });
    }

    async fn sync_outbound(&self, connection: Connection) -> Result<(), SwarmError> {
        sync_outbound(connection, self.store.clone(), self.changes.clone(), self.view_changes.clone()).await
    }

    fn sync_known_nodes(&self) {
        let endpoint = self.endpoint.clone();
        let store = self.store.clone();
        let changes = self.changes.clone();
        let view_changes = self.view_changes.clone();
        tokio::spawn(async move {
            let nodes = match store.view().await { Ok(view) => view.membership.nodes, Err(_) => return };
            for node in nodes.into_values().filter(|node| node.endpoint_id != endpoint.id()) {
                let Ok(connection) = endpoint.connect(node.endpoint_addr, ALPN).await else { continue; };
                let _ = sync_outbound(connection, store.clone(), changes.clone(), view_changes.clone()).await;
            }
        });
    }
}

const MAX_LOG_BYTES: usize = 16 * 1024 * 1024;

async fn sync_outbound(connection: Connection, store: Arc<dyn Store>, changes: broadcast::Sender<MembershipView>, view_changes: broadcast::Sender<SwarmView>) -> Result<(), SwarmError> {
    let (mut send, mut recv) = connection.open_bi().await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let bytes = serde_cbor::to_vec(&store.commits().await.map_err(SwarmError::Store)?).expect("commit serialization cannot fail");
    send.write_all(&bytes).await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    send.finish().map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let remote = recv.read_to_end(MAX_LOG_BYTES).await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    merge_and_publish(serde_cbor::from_slice(&remote).map_err(|error| SwarmError::Iroh(Box::new(error)))?, store, changes, view_changes).await
}

async fn sync_inbound(connection: Connection, store: Arc<dyn Store>, changes: broadcast::Sender<MembershipView>, view_changes: broadcast::Sender<SwarmView>) -> Result<(), SwarmError> {
    let (mut send, mut recv) = connection.accept_bi().await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let remote = recv.read_to_end(MAX_LOG_BYTES).await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    merge_and_publish(serde_cbor::from_slice(&remote).map_err(|error| SwarmError::Iroh(Box::new(error)))?, store.clone(), changes.clone(), view_changes.clone()).await?;
    let bytes = serde_cbor::to_vec(&store.commits().await.map_err(SwarmError::Store)?).expect("commit serialization cannot fail");
    send.write_all(&bytes).await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    send.finish().map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    Ok(())
}

async fn merge_and_publish(remote: Vec<Commit>, store: Arc<dyn Store>, changes: broadcast::Sender<MembershipView>, view_changes: broadcast::Sender<SwarmView>) -> Result<(), SwarmError> {
    let changed = store.merge(remote).await.map_err(SwarmError::Store)?;
    if changed {
        let view = store.view().await.map_err(SwarmError::Store)?;
        let _ = changes.send(view.membership.clone());
        let _ = view_changes.send(view);
    }
    Ok(())
}

#[derive(Deserialize, Serialize)]
struct ServiceHello { path: ServicePath, user: UserId }
#[derive(Deserialize, Serialize)]
struct ServiceChallenge { nonce: [u8; 32] }
#[derive(Deserialize, Serialize)]
struct ServiceProof { signature: Vec<u8> }

fn auth_bytes(path: &ServicePath, nonce: &[u8; 32]) -> Vec<u8> {
    serde_cbor::to_vec(&(b"atlas-swarm/service-auth/1", path, nonce)).expect("authentication serialization cannot fail")
}

async fn authenticate_client(connection: &Connection, path: &ServicePath, key: &SigningKey) -> Result<(), SwarmError> {
    let (mut send, mut recv) = connection.open_bi().await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    write_frame(&mut send, &ServiceHello { path: path.clone(), user: UserId::from_signing_key(key) }).await?;
    let challenge: ServiceChallenge = read_frame(&mut recv).await?;
    let signature = ed25519_dalek::Signer::sign(key, &auth_bytes(path, &challenge.nonce)).to_bytes().to_vec();
    write_frame(&mut send, &ServiceProof { signature }).await?;
    send.finish().map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let accepted: bool = read_frame(&mut recv).await?;
    accepted.then_some(()).ok_or(SwarmError::AuthenticationFailed)
}

async fn accept_service(connection: Connection, services: Arc<RwLock<BTreeMap<ServicePath, ServiceRegistrar>>>, store: Arc<dyn Store>) -> Result<(), SwarmError> {
    let (mut send, mut recv) = connection.accept_bi().await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let hello: ServiceHello = read_frame(&mut recv).await?;
    let mut nonce = [0; 32];
    rand::thread_rng().fill(&mut nonce);
    write_frame(&mut send, &ServiceChallenge { nonce }).await?;
    let proof: ServiceProof = read_frame(&mut recv).await?;
    let allowed = store.view().await.map_err(SwarmError::Store)?.services.get(&hello.path).is_some_and(|service| {
        service.allowed_users.contains(&hello.user) && hello.user.verifying_key().is_some_and(|key| {
            proof.signature.as_slice().try_into().is_ok_and(|signature| ed25519_dalek::Verifier::verify(&key, &auth_bytes(&hello.path, &nonce), &ed25519_dalek::Signature::from_bytes(signature)).is_ok())
        })
    });
    write_frame(&mut send, &allowed).await?;
    send.finish().map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    if !allowed { return Err(SwarmError::AuthenticationFailed); }
    let registrar = services.read().await.get(&hello.path).cloned().ok_or_else(|| SwarmError::ServiceUnavailable(hello.path.as_str().into()))?;
    let (send, recv) = connection.accept_bi().await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    registrar(atlas_rpc::Peer::new(atlas_rpc::CborTransport(IrohTransport::new(send, recv))));
    Ok(())
}

async fn write_frame<T: Serialize>(send: &mut SendStream, value: &T) -> Result<(), SwarmError> {
    let bytes = serde_cbor::to_vec(value).expect("frame serialization cannot fail");
    let length = u32::try_from(bytes.len()).map_err(|_| SwarmError::AuthenticationFailed)?.to_be_bytes();
    send.write_all(&length).await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    send.write_all(&bytes).await.map_err(|error| SwarmError::Iroh(Box::new(error)))
}

async fn read_frame<T: for<'de> Deserialize<'de>>(recv: &mut RecvStream) -> Result<T, SwarmError> {
    let mut length = [0; 4];
    recv.read_exact(&mut length).await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    let mut bytes = vec![0; u32::from_be_bytes(length) as usize];
    recv.read_exact(&mut bytes).await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
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
                if send.write_all(&length).await.is_err() || send.write_all(&bytes).await.is_err() { break; }
            }
            let _ = send.finish();
        });
        tokio::spawn(async move {
            loop {
                let mut length = [0; 4];
                if recv.read_exact(&mut length).await.is_err() { break; }
                let mut bytes = vec![0; u32::from_be_bytes(length) as usize];
                if recv.read_exact(&mut bytes).await.is_err() { break; }
                if reads.send(Ok(bytes::Bytes::from(bytes))).is_err() { break; }
            }
        });
        Self { incoming, outgoing }
    }
}

impl Stream for IrohTransport {
    type Item = Result<bytes::Bytes, io::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> { self.incoming.poll_recv(cx) }
}

impl Sink<bytes::Bytes> for IrohTransport {
    type Error = io::Error;
    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> { Poll::Ready(Ok(())) }
    fn start_send(self: Pin<&mut Self>, item: bytes::Bytes) -> Result<(), Self::Error> { self.outgoing.send(item).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "Iroh stream closed")) }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> { Poll::Ready(Ok(())) }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> { Poll::Ready(Ok(())) }
}
