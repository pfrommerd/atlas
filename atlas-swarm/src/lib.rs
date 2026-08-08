//! Eventually consistent membership for a small swarm of Iroh endpoints.

mod log;
mod store;
mod topology;

use std::{collections::BTreeSet, sync::Arc};

use iroh::{endpoint::presets, Endpoint, EndpointAddr, SecretKey};
use rand::Rng;
use thiserror::Error;
use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;

pub use log::{membership_view, Commit, CommitId, MembershipOperation, MembershipView, NodeCoordinate, NodeRecord};
pub use store::{MemoryStore, Store, StoredIdentity};
pub use topology::neighbors;

pub const ALPN: &[u8] = b"atlas-swarm/1";

#[derive(Debug, Error)]
pub enum SwarmError {
    #[error("the node name must not be empty")]
    EmptyNodeName,
    #[error("store error: {0}")]
    Store(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("Iroh error: {0}")]
    Iroh(#[source] Box<dyn std::error::Error + Send + Sync>),
}

pub struct Swarm {
    endpoint: Endpoint,
    store: Arc<dyn Store>,
    identity: StoredIdentity,
    commits: RwLock<Vec<Commit>>,
    changes: broadcast::Sender<MembershipView>,
}

impl Swarm {
    pub async fn create(node_name: impl Into<String>, store: Arc<dyn Store>) -> Result<Self, SwarmError> {
        Self::open(node_name.into(), store, Uuid::new_v4()).await
    }

    pub async fn join(node_name: impl Into<String>, bootstrap: EndpointAddr, store: Arc<dyn Store>) -> Result<Self, SwarmError> {
        let swarm = Self::open(node_name.into(), store, Uuid::new_v4()).await?;
        swarm.endpoint.connect(bootstrap, ALPN).await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
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
        let endpoint = Endpoint::builder(presets::N0).secret_key(key).alpns(vec![ALPN.to_vec()]).bind().await.map_err(|error| SwarmError::Iroh(Box::new(error)))?;
        let mut commits = store.commits().await.map_err(SwarmError::Store)?;
        if commits.is_empty() {
            let commit = Commit::new(BTreeSet::new(), endpoint.id(), MembershipOperation::Join(NodeRecord { name: identity.node_name.clone(), endpoint_id: endpoint.id(), coordinate: identity.coordinate }), endpoint.secret_key());
            store.append(commit.clone()).await.map_err(SwarmError::Store)?;
            commits.push(commit);
        }
        let (changes, _) = broadcast::channel(64);
        Ok(Self { endpoint, store, identity, commits: RwLock::new(commits), changes })
    }

    pub fn endpoint_id(&self) -> iroh::EndpointId { self.endpoint.id() }
    pub fn endpoint_addr(&self) -> EndpointAddr { self.endpoint.addr() }
    pub fn swarm_id(&self) -> Uuid { self.identity.swarm_id }
    pub fn node_name(&self) -> &str { &self.identity.node_name }
    pub fn store(&self) -> &Arc<dyn Store> { &self.store }
    pub fn subscribe(&self) -> broadcast::Receiver<MembershipView> { self.changes.subscribe() }
    pub async fn membership(&self) -> MembershipView { membership_view(self.commits.read().await.clone()) }
}
