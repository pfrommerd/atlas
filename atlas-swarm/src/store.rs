use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{Commit, CommitId, NodeCoordinate};

#[derive(Clone, Debug, PartialEq)]
pub struct StoredIdentity {
    pub swarm_id: Uuid,
    pub secret_key: [u8; 32],
    pub node_name: String,
    pub coordinate: NodeCoordinate,
}

#[async_trait]
pub trait Store: Send + Sync + 'static {
    async fn load_identity(&self) -> Result<Option<StoredIdentity>, Box<dyn std::error::Error + Send + Sync>>;
    async fn save_identity(&self, identity: StoredIdentity) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn commits(&self) -> Result<Vec<Commit>, Box<dyn std::error::Error + Send + Sync>>;
    async fn append(&self, commit: Commit) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>;
}

#[derive(Clone, Default)]
pub struct MemoryStore(Arc<Mutex<MemoryState>>);

#[derive(Default)]
struct MemoryState {
    identity: Option<StoredIdentity>,
    commits: BTreeMap<CommitId, Commit>,
}

#[async_trait]
impl Store for MemoryStore {
    async fn load_identity(&self) -> Result<Option<StoredIdentity>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.0.lock().await.identity.clone())
    }

    async fn save_identity(&self, identity: StoredIdentity) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.0.lock().await.identity = Some(identity);
        Ok(())
    }

    async fn commits(&self) -> Result<Vec<Commit>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.0.lock().await.commits.values().cloned().collect())
    }

    async fn append(&self, commit: Commit) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.0.lock().await;
        if state.commits.contains_key(&commit.id) {
            return Ok(false);
        }
        state.commits.insert(commit.id, commit);
        Ok(true)
    }
}
