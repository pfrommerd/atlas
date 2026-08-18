use std::{collections::BTreeSet, path::Path, sync::Arc};

use redb::{Database, ReadableDatabase, TableDefinition};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{RepositoryId, RepositorySnapshotId};

const OBJECTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("repository_objects");
const SNAPSHOTS: TableDefinition<&[u8], &[u8]> = TableDefinition::new("repository_snapshots");
const REPLICATION_JOBS: TableDefinition<&[u8], &[u8]> =
    TableDefinition::new("repository_replication_jobs");

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[repr(u8)]
pub enum ObjectKind {
    Commit = 1,
    Tree = 2,
    File = 3,
    Symlink = 4,
    Conflict = 5,
    Operation = 6,
    View = 7,
}

/// Workspace-neutral jj state referenced by the Atlas metadata log.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JujutsuSnapshot {
    pub format_version: u32,
    pub operation_heads: BTreeSet<Vec<u8>>,
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
        let hash: [u8; 32] = Sha256::digest(bytes).into();
        let key = object_key(repository_id, kind, hash);
        let write = self.0.begin_write()?;
        write.open_table(OBJECTS)?.insert(key.as_slice(), bytes)?;
        write.commit()?;
        Ok(hash)
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
            .get(object_key(repository_id, kind, hash).as_slice())?
            .map(|value| value.value().to_vec()))
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
}

fn object_key(repository_id: RepositoryId, kind: ObjectKind, hash: [u8; 32]) -> Vec<u8> {
    let mut key = Vec::with_capacity(49);
    key.extend_from_slice(repository_id.as_bytes());
    key.push(kind as u8);
    key.extend_from_slice(&hash);
    key
}
