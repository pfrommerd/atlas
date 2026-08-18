use std::{collections::BTreeMap, path::Path, sync::Arc};

use async_trait::async_trait;
use iroh::SecretKey;
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};

use crate::{Commit, CommitId, Store, StoredIdentity, SwarmView, store::resolve_view};

const IDENTITY: TableDefinition<&str, &[u8]> = TableDefinition::new("swarm_identity");
const COMMITS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("swarm_commits");

/// Durable storage for the replicated Atlas metadata log.
#[derive(Clone)]
pub struct RedbStore(Arc<Database>);

impl RedbStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, redb::DatabaseError> {
        Ok(Self(Arc::new(Database::create(path)?)))
    }

    pub fn open_with_repository(
        path: impl AsRef<Path>,
    ) -> Result<(Self, crate::repository::RepositoryDatabase), redb::DatabaseError> {
        let database = Arc::new(Database::create(path)?);
        Ok((
            Self(database.clone()),
            crate::repository::RepositoryDatabase::from_database(database),
        ))
    }

    fn read_commits(
        &self,
    ) -> Result<BTreeMap<CommitId, Commit>, Box<dyn std::error::Error + Send + Sync>> {
        let read = self.0.begin_read()?;
        let table = match read.open_table(COMMITS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(BTreeMap::new()),
            Err(error) => return Err(error.into()),
        };
        let mut commits = BTreeMap::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            let commit: Commit = serde_cbor::from_slice(value.value())?;
            commits.insert(commit.id, commit);
        }
        Ok(commits)
    }
}

#[async_trait]
impl Store for RedbStore {
    async fn load_identity(
        &self,
    ) -> Result<Option<StoredIdentity>, Box<dyn std::error::Error + Send + Sync>> {
        let read = self.0.begin_read()?;
        let table = match read.open_table(IDENTITY) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let identity = table
            .get("identity")?
            .map(|value| serde_cbor::from_slice(value.value()))
            .transpose()?;
        Ok(identity)
    }

    async fn save_identity(
        &self,
        identity: StoredIdentity,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let bytes = serde_cbor::to_vec(&identity)?;
        let write = self.0.begin_write()?;
        write
            .open_table(IDENTITY)?
            .insert("identity", bytes.as_slice())?;
        write.commit()?;
        Ok(())
    }

    async fn commits(&self) -> Result<Vec<Commit>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.read_commits()?.into_values().collect())
    }

    async fn append_commit(
        &self,
        mut commit: Commit,
        key: &SecretKey,
    ) -> Result<Commit, Box<dyn std::error::Error + Send + Sync>> {
        if commit.author != key.public()
            || !commit.endpoint_signature.is_empty()
            || !commit.verify_user()
        {
            return Err("invalid local commit signature".into());
        }
        let mut all = self.read_commits()?;
        if all.contains_key(&commit.id) {
            return Err("duplicate commit id".into());
        }
        commit.sign_endpoint(key);
        all.insert(commit.id, commit.clone());
        if !crate::store::valid_history(&all) {
            return Err("invalid commit ancestry".into());
        }
        let bytes = serde_cbor::to_vec(&commit)?;
        let write = self.0.begin_write()?;
        write
            .open_table(COMMITS)?
            .insert(commit.id.as_bytes().as_slice(), bytes.as_slice())?;
        write.commit()?;
        Ok(commit)
    }

    async fn merge(
        &self,
        commits: Vec<Commit>,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let mut all = self.read_commits()?;
        let old_len = all.len();
        for commit in &commits {
            if !commit.verify() {
                return Err("invalid replicated commit signature".into());
            }
            all.entry(commit.id).or_insert_with(|| commit.clone());
        }
        if !crate::store::valid_history(&all) {
            return Err("invalid replicated commit ancestry".into());
        }
        if all.len() == old_len {
            return Ok(false);
        }
        let write = self.0.begin_write()?;
        {
            let mut table = write.open_table(COMMITS)?;
            for commit in commits {
                let bytes = serde_cbor::to_vec(&commit)?;
                table.insert(commit.id.as_bytes().as_slice(), bytes.as_slice())?;
            }
        }
        write.commit()?;
        Ok(true)
    }

    async fn view(&self) -> Result<SwarmView, Box<dyn std::error::Error + Send + Sync>> {
        Ok(resolve_view(self.read_commits()?.into_values()))
    }
}
