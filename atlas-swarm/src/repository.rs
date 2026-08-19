use std::{collections::BTreeSet, path::Path, sync::Arc};

use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{RepositoryId, RepositorySnapshotId};

const OBJECTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("repository_objects");
const SNAPSHOTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("repository_snapshots");
const REPLICATION_JOBS: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("repository_replication_jobs");
const CHECKOUT_OBJECTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("checkout_objects");
const CHECKOUT_OP_HEADS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("checkout_op_heads");

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct CheckoutId(pub uuid::Uuid);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[repr(u8)]
pub enum CheckoutObjectKind {
    Operation = 1,
    View = 2,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[repr(u8)]
pub enum ObjectKind {
    Commit = 1,
    Tree = 2,
    File = 3,
    Symlink = 4,
    Conflict = 5,
    Copy = 6,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct RepositoryObjectId {
    pub kind: ObjectKind,
    pub id: Vec<u8>,
}

/// Workspace-neutral jj state referenced by the Atlas metadata log.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JujutsuSnapshot {
    pub format_version: u32,
    #[serde(default)]
    pub parents: BTreeSet<RepositorySnapshotId>,
    /// Atlas-native encoding of jj's workspace-neutral repository view.
    pub view: Vec<u8>,
    /// The complete object closure needed to materialize this view.
    #[serde(default)]
    pub objects: BTreeSet<RepositoryObjectId>,
}

impl JujutsuSnapshot {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.format_version != crate::JJ_REPOSITORY_FORMAT_VERSION {
            return Err("unsupported jj repository snapshot format");
        }
        if self.view.is_empty() || self.objects.iter().any(|object| object.id.is_empty()) {
            return Err("repository snapshot has an invalid object closure");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReplicationJob {
    pub repository_id: RepositoryId,
    pub snapshot: RepositorySnapshotId,
    pub pending_endpoints: BTreeSet<iroh::EndpointId>,
}

/// Content-addressed objects and durable replication work. This database is
/// intentionally separate from `RedbStore`, but callers may pass the same path
/// when they explicitly want a combined database.
#[derive(Clone)]
pub struct RepositoryDatabase(Arc<Database>);

impl RepositoryDatabase {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, redb::DatabaseError> {
        Ok(Self(Arc::new(Database::create(path)?)))
    }

    pub(crate) fn from_database(database: Arc<Database>) -> Self {
        Self(database)
    }

    pub fn put_object(
        &self,
        repository_id: RepositoryId,
        kind: ObjectKind,
        bytes: &[u8],
    ) -> Result<[u8; 32], Box<dyn std::error::Error + Send + Sync>> {
        let hash = repository_object_hash(kind, bytes);
        self.put_object_with_id(repository_id, kind, &hash, bytes)?;
        Ok(hash)
    }

    pub fn put_object_with_id(
        &self,
        repository_id: RepositoryId,
        kind: ObjectKind,
        id: &[u8],
        bytes: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if id != repository_object_hash(kind, bytes) {
            return Err("repository object id does not match its typed content hash".into());
        }
        let key = object_key(repository_id, kind, id);
        let write = self.0.begin_write()?;
        {
            let mut table = write.open_table(OBJECTS)?;
            if let Some(existing) = table.get(key.as_slice())? {
                if existing.value() != bytes {
                    return Err("repository object id is already bound to different bytes".into());
                }
                return Ok(());
            }
            table.insert(key.as_slice(), bytes)?;
        }
        write.commit()?;
        Ok(())
    }

    pub fn get_object(
        &self,
        repository_id: RepositoryId,
        kind: ObjectKind,
        hash: [u8; 32],
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        let read = self.0.begin_read()?;
        let table = match read.open_table(OBJECTS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(table
            .get(object_key(repository_id, kind, &hash).as_slice())?
            .map(|value| value.value().to_vec()))
    }

    pub fn get_object_by_id(
        &self,
        repository_id: RepositoryId,
        object: &RepositoryObjectId,
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        let read = self.0.begin_read()?;
        let table = match read.open_table(OBJECTS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(table
            .get(object_key(repository_id, object.kind, &object.id).as_slice())?
            .map(|value| value.value().to_vec()))
    }

    pub fn has_object(
        &self,
        repository_id: RepositoryId,
        object: &RepositoryObjectId,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.get_object_by_id(repository_id, object)?.is_some())
    }

    pub fn put_checkout_object(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        kind: CheckoutObjectKind,
        id: &[u8],
        bytes: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let key = checkout_object_key(repository_id, checkout_id, kind, id);
        let write = self.0.begin_write()?;
        {
            let mut table = write.open_table(CHECKOUT_OBJECTS)?;
            if let Some(existing) = table.get(key.as_slice())? {
                if existing.value() != bytes {
                    return Err("checkout object id is already bound to different bytes".into());
                }
                return Ok(());
            }
            table.insert(key.as_slice(), bytes)?;
        }
        write.commit()?;
        Ok(())
    }

    pub fn get_checkout_object(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        kind: CheckoutObjectKind,
        id: &[u8],
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        let read = self.0.begin_read()?;
        let table = match read.open_table(CHECKOUT_OBJECTS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(table
            .get(checkout_object_key(repository_id, checkout_id, kind, id).as_slice())?
            .map(|value| value.value().to_vec()))
    }

    pub fn checkout_object_ids(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        kind: CheckoutObjectKind,
    ) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        let mut prefix = repository_id.as_bytes().to_vec();
        prefix.extend_from_slice(checkout_id.0.as_bytes());
        prefix.push(kind as u8);
        let read = self.0.begin_read()?;
        let table = match read.open_table(CHECKOUT_OBJECTS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut ids = Vec::new();
        for entry in table.range(prefix.as_slice()..)? {
            let (key, _) = entry?;
            let key = key.value();
            if !key.starts_with(&prefix) {
                break;
            }
            ids.push(key[prefix.len()..].to_vec());
        }
        Ok(ids)
    }

    /// Atomically publishes `new_id` and removes only the operation heads the
    /// caller observed. Concurrent writers therefore produce divergent heads
    /// instead of excluding one another; jj resolves those on the next load.
    pub fn update_checkout_op_heads(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        old_ids: &[Vec<u8>],
        new_id: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let prefix = checkout_prefix(repository_id, checkout_id);
        let write = self.0.begin_write()?;
        {
            let mut table = write.open_table(CHECKOUT_OP_HEADS)?;
            let mut new_key = prefix.clone();
            new_key.extend_from_slice(new_id);
            table.insert(new_key.as_slice(), &[] as &[u8])?;
            for old_id in old_ids {
                if old_id != new_id {
                    let mut old_key = prefix.clone();
                    old_key.extend_from_slice(old_id);
                    table.remove(old_key.as_slice())?;
                }
            }
        }
        write.commit()?;
        Ok(())
    }

    pub fn checkout_op_heads(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
    ) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        let prefix = checkout_prefix(repository_id, checkout_id);
        let read = self.0.begin_read()?;
        let table = match read.open_table(CHECKOUT_OP_HEADS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut heads = Vec::new();
        for entry in table.range(prefix.as_slice()..)? {
            let (key, _) = entry?;
            let key = key.value();
            if !key.starts_with(&prefix) {
                break;
            }
            heads.push(key[prefix.len()..].to_vec());
        }
        Ok(heads)
    }

    pub fn write_snapshot(
        &self,
        repository_id: RepositoryId,
        snapshot: &JujutsuSnapshot,
    ) -> Result<RepositorySnapshotId, Box<dyn std::error::Error + Send + Sync>> {
        let bytes = serde_cbor::to_vec(snapshot)?;
        let id = RepositorySnapshotId(Sha256::digest(&bytes).into());
        let mut key = repository_id.as_bytes().to_vec();
        key.extend_from_slice(&id.0);
        let write = self.0.begin_write()?;
        write
            .open_table(SNAPSHOTS)?
            .insert(key.as_slice(), bytes.as_slice())?;
        write.commit()?;
        Ok(id)
    }

    pub fn read_snapshot(
        &self,
        repository_id: RepositoryId,
        snapshot_id: &RepositorySnapshotId,
    ) -> Result<Option<JujutsuSnapshot>, Box<dyn std::error::Error + Send + Sync>> {
        let mut key = repository_id.as_bytes().to_vec();
        key.extend_from_slice(&snapshot_id.0);
        let read = self.0.begin_read()?;
        let table = match read.open_table(SNAPSHOTS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        table
            .get(key.as_slice())?
            .map(|value| serde_cbor::from_slice(value.value()).map_err(Into::into))
            .transpose()
    }

    pub fn enqueue_replication(
        &self,
        job: &ReplicationJob,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let bytes = serde_cbor::to_vec(job)?;
        let mut key = job.repository_id.as_bytes().to_vec();
        key.extend_from_slice(&job.snapshot.0);
        let write = self.0.begin_write()?;
        write
            .open_table(REPLICATION_JOBS)?
            .insert(key.as_slice(), bytes.as_slice())?;
        write.commit()?;
        Ok(())
    }

    pub fn replication_jobs(
        &self,
    ) -> Result<Vec<ReplicationJob>, Box<dyn std::error::Error + Send + Sync>> {
        let read = self.0.begin_read()?;
        let table = match read.open_table(REPLICATION_JOBS) {
            Ok(table) => table,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut jobs = Vec::new();
        for entry in table.iter()? {
            let (_, value) = entry?;
            jobs.push(serde_cbor::from_slice(value.value())?);
        }
        Ok(jobs)
    }

    pub fn complete_replication_endpoint(
        &self,
        repository_id: RepositoryId,
        snapshot: &RepositorySnapshotId,
        endpoint: iroh::EndpointId,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut key = repository_id.as_bytes().to_vec();
        key.extend_from_slice(&snapshot.0);
        let write = self.0.begin_write()?;
        {
            let mut table = write.open_table(REPLICATION_JOBS)?;
            let Some(value) = table.get(key.as_slice())? else {
                return Ok(());
            };
            let mut job: ReplicationJob = serde_cbor::from_slice(value.value())?;
            drop(value);
            job.pending_endpoints.remove(&endpoint);
            if job.pending_endpoints.is_empty() {
                table.remove(key.as_slice())?;
            } else {
                let bytes = serde_cbor::to_vec(&job)?;
                table.insert(key.as_slice(), bytes.as_slice())?;
            }
        }
        write.commit()?;
        Ok(())
    }
}

/// Versioned, typed repository-object identity. Including the object kind
/// prevents cross-type aliases and the format tag makes incompatible future
/// encodings explicit.
pub fn repository_object_hash(kind: ObjectKind, bytes: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"atlas-jj-object-v2\0");
    hash.update([kind as u8]);
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
    hash.finalize().into()
}

fn object_key(repository_id: RepositoryId, kind: ObjectKind, id: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(17 + id.len());
    key.extend_from_slice(repository_id.as_bytes());
    key.push(kind as u8);
    key.extend_from_slice(id);
    key
}

fn checkout_object_key(
    repository_id: RepositoryId,
    checkout_id: CheckoutId,
    kind: CheckoutObjectKind,
    id: &[u8],
) -> Vec<u8> {
    let mut key = repository_id.as_bytes().to_vec();
    key.extend_from_slice(checkout_id.0.as_bytes());
    key.push(kind as u8);
    key.extend_from_slice(id);
    key
}

fn checkout_prefix(repository_id: RepositoryId, checkout_id: CheckoutId) -> Vec<u8> {
    let mut key = repository_id.as_bytes().to_vec();
    key.extend_from_slice(checkout_id.0.as_bytes());
    key
}
