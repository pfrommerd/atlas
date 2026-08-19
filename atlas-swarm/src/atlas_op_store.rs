use std::{
    collections::BTreeMap,
    fmt::{Debug, Formatter},
    path::Path,
    sync::Arc,
    time::SystemTime,
};

use async_trait::async_trait;
use jj_lib::{
    backend::{CommitId, MillisSinceEpoch, Timestamp},
    merge::Merge,
    object_id::{HexPrefix, ObjectId, PrefixResolution},
    op_store::{
        OpStore, OpStoreError, OpStoreResult, Operation, OperationId, OperationMetadata, RefTarget,
        RemoteRef, RemoteRefState, RemoteView, RootOperationData, TimestampRange, View, ViewId,
    },
    ref_name::{GitRefNameBuf, RefNameBuf, RemoteNameBuf, WorkspaceNameBuf},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    RepositoryId, UserId,
    local::{
        CheckoutObjectIdsRequest, CheckoutObjectRequest, CheckoutOpHeadsRequest,
        PutCheckoutObjectRequest, UpdateCheckoutOpHeadsRequest, connect_control,
    },
    repository::{CheckoutId, CheckoutObjectKind, RepositoryDatabase},
};

const ID_LENGTH: usize = 32;

#[async_trait]
pub trait CheckoutObjectStore: Send + Sync {
    async fn get(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        kind: CheckoutObjectKind,
        id: &[u8],
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>>;
    async fn put(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        kind: CheckoutObjectKind,
        id: &[u8],
        bytes: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn ids(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        kind: CheckoutObjectKind,
    ) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>>;
    async fn op_heads(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
    ) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>>;
    async fn update_op_heads(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        old_ids: &[Vec<u8>],
        new_id: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}

#[async_trait]
impl CheckoutObjectStore for RepositoryDatabase {
    async fn get(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        kind: CheckoutObjectKind,
        id: &[u8],
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        self.get_checkout_object(repository_id, checkout_id, kind, id)
    }
    async fn put(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        kind: CheckoutObjectKind,
        id: &[u8],
        bytes: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.put_checkout_object(repository_id, checkout_id, kind, id, bytes)
    }
    async fn ids(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        kind: CheckoutObjectKind,
    ) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        self.checkout_object_ids(repository_id, checkout_id, kind)
    }
    async fn op_heads(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
    ) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        self.checkout_op_heads(repository_id, checkout_id)
    }
    async fn update_op_heads(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        old_ids: &[Vec<u8>],
        new_id: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.update_checkout_op_heads(repository_id, checkout_id, old_ids, new_id)
    }
}

#[derive(Clone, Debug)]
pub struct RpcCheckoutObjectStore {
    socket: std::path::PathBuf,
    user: UserId,
}

impl RpcCheckoutObjectStore {
    pub fn new(socket: std::path::PathBuf, user: UserId) -> Self {
        Self { socket, user }
    }
}

#[async_trait]
impl CheckoutObjectStore for RpcCheckoutObjectStore {
    async fn get(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        kind: CheckoutObjectKind,
        id: &[u8],
    ) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        connect_control(&self.socket)
            .await?
            .get_checkout_object(CheckoutObjectRequest {
                repository_id,
                checkout_id,
                user: self.user,
                kind,
                id: id.to_vec(),
            })
            .await
            .map_err(|error| std::io::Error::other(error).into())
    }
    async fn put(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        kind: CheckoutObjectKind,
        id: &[u8],
        bytes: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        connect_control(&self.socket)
            .await?
            .put_checkout_object(PutCheckoutObjectRequest {
                repository_id,
                checkout_id,
                user: self.user,
                kind,
                id: id.to_vec(),
                bytes: bytes.to_vec(),
            })
            .await
            .map_err(|error| std::io::Error::other(error).into())
    }
    async fn ids(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        kind: CheckoutObjectKind,
    ) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        connect_control(&self.socket)
            .await?
            .checkout_object_ids(CheckoutObjectIdsRequest {
                repository_id,
                checkout_id,
                user: self.user,
                kind,
            })
            .await
            .map_err(|error| std::io::Error::other(error).into())
    }
    async fn op_heads(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
    ) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error + Send + Sync>> {
        connect_control(&self.socket)
            .await?
            .checkout_op_heads(CheckoutOpHeadsRequest {
                repository_id,
                checkout_id,
                user: self.user,
            })
            .await
            .map_err(|error| std::io::Error::other(error).into())
    }
    async fn update_op_heads(
        &self,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        old_ids: &[Vec<u8>],
        new_id: &[u8],
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        connect_control(&self.socket)
            .await?
            .update_checkout_op_heads(UpdateCheckoutOpHeadsRequest {
                repository_id,
                checkout_id,
                user: self.user,
                old_ids: old_ids.to_vec(),
                new_id: new_id.to_vec(),
            })
            .await
            .map_err(|error| std::io::Error::other(error).into())
    }
}

/// Per-checkout operation storage. Objects live in local daemon tables and are
/// never part of repository snapshots or replication.
pub struct AtlasOpStore {
    root_data: RootOperationData,
    root_operation_id: OperationId,
    root_view_id: ViewId,
    repository_id: RepositoryId,
    checkout_id: CheckoutId,
    objects: Arc<dyn CheckoutObjectStore>,
}

impl Debug for AtlasOpStore {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AtlasOpStore")
            .field("repository_id", &self.repository_id)
            .field("checkout_id", &self.checkout_id)
            .finish()
    }
}

impl AtlasOpStore {
    pub const NAME: &'static str = "atlas";

    pub fn new(
        root_data: RootOperationData,
        repository_id: RepositoryId,
        checkout_id: CheckoutId,
        objects: Arc<dyn CheckoutObjectStore>,
    ) -> Self {
        Self {
            root_data,
            root_operation_id: OperationId::new(vec![0; ID_LENGTH]),
            root_view_id: ViewId::new(vec![0; ID_LENGTH]),
            repository_id,
            checkout_id,
            objects,
        }
    }

    /// Compatibility constructor for direct-store tests. The path only scopes
    /// a checkout ID; no files or directories are created there.
    pub fn init(
        path: &Path,
        root_data: RootOperationData,
        repository_id: RepositoryId,
        objects: Arc<dyn CheckoutObjectStore>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut hash = Sha256::new();
        hash.update(b"atlas-checkout-test\0");
        hash.update(repository_id.as_bytes());
        hash.update(path.as_os_str().as_encoded_bytes());
        let digest: [u8; 32] = hash.finalize().into();
        Ok(Self::new(
            root_data,
            repository_id,
            CheckoutId(uuid::Uuid::from_bytes(digest[..16].try_into().unwrap())),
            objects,
        ))
    }

    async fn read(&self, kind: CheckoutObjectKind, id: &impl ObjectId) -> OpStoreResult<Vec<u8>> {
        self.objects
            .get(self.repository_id, self.checkout_id, kind, id.as_bytes())
            .await
            .map_err(OpStoreError::Other)?
            .ok_or_else(|| OpStoreError::ObjectNotFound {
                object_type: id.object_type(),
                hash: id.hex(),
                source: "object is absent from the local Atlas checkout".into(),
            })
    }

    async fn write(&self, kind: CheckoutObjectKind, bytes: &[u8]) -> OpStoreResult<Vec<u8>> {
        let id = local_hash(kind, bytes).to_vec();
        self.objects
            .put(self.repository_id, self.checkout_id, kind, &id, bytes)
            .await
            .map_err(OpStoreError::Other)?;
        Ok(id)
    }
}

#[async_trait]
impl OpStore for AtlasOpStore {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn root_operation_id(&self) -> &OperationId {
        &self.root_operation_id
    }

    async fn read_view(&self, id: &ViewId) -> OpStoreResult<View> {
        if id == &self.root_view_id {
            return Ok(View::make_root(self.root_data.root_commit_id.clone()));
        }
        let wire: ViewWire =
            serde_cbor::from_slice(&self.read(CheckoutObjectKind::View, id).await?)
                .map_err(|error| OpStoreError::Other(error.into()))?;
        wire.try_into().map_err(OpStoreError::Other)
    }

    async fn write_view(&self, contents: &View) -> OpStoreResult<ViewId> {
        let bytes = serde_cbor::to_vec(&ViewWire::from(contents))
            .map_err(|error| OpStoreError::Other(error.into()))?;
        Ok(ViewId::new(
            self.write(CheckoutObjectKind::View, &bytes).await?,
        ))
    }

    async fn read_operation(&self, id: &OperationId) -> OpStoreResult<Operation> {
        if id == &self.root_operation_id {
            return Ok(Operation::make_root(self.root_view_id.clone()));
        }
        let wire: OperationWire =
            serde_cbor::from_slice(&self.read(CheckoutObjectKind::Operation, id).await?)
                .map_err(|error| OpStoreError::Other(error.into()))?;
        Ok(wire.into())
    }

    async fn write_operation(&self, contents: &Operation) -> OpStoreResult<OperationId> {
        if contents.parents.is_empty() {
            return Err(OpStoreError::Other(
                "non-root operation has no parents".into(),
            ));
        }
        let bytes = serde_cbor::to_vec(&OperationWire::from(contents))
            .map_err(|error| OpStoreError::Other(error.into()))?;
        Ok(OperationId::new(
            self.write(CheckoutObjectKind::Operation, &bytes).await?,
        ))
    }

    async fn resolve_operation_id_prefix(
        &self,
        prefix: &HexPrefix,
    ) -> OpStoreResult<PrefixResolution<OperationId>> {
        let mut matches = self
            .objects
            .ids(
                self.repository_id,
                self.checkout_id,
                CheckoutObjectKind::Operation,
            )
            .await
            .map_err(OpStoreError::Other)?
            .into_iter()
            .map(OperationId::new)
            .filter(|id| prefix.matches(id));
        let root = prefix
            .matches(&self.root_operation_id)
            .then(|| self.root_operation_id.clone());
        let first = root.or_else(|| matches.next());
        Ok(match (first, matches.next()) {
            (None, _) => PrefixResolution::NoMatch,
            (Some(id), None) => PrefixResolution::SingleMatch(id),
            (Some(_), Some(_)) => PrefixResolution::AmbiguousMatch,
        })
    }

    async fn gc(&self, _head_ids: &[OperationId], _keep_newer: SystemTime) -> OpStoreResult<()> {
        Ok(())
    }
}

fn local_hash(kind: CheckoutObjectKind, bytes: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"atlas-jj-local-v2\0");
    hash.update([kind as u8]);
    hash.update(bytes);
    hash.finalize().into()
}

#[derive(Clone, Serialize, Deserialize)]
struct TargetWire(Vec<Option<Vec<u8>>>);

impl From<&RefTarget> for TargetWire {
    fn from(value: &RefTarget) -> Self {
        Self(
            value
                .as_merge()
                .iter()
                .map(|term| term.as_ref().map(ObjectId::to_bytes))
                .collect(),
        )
    }
}
impl TryFrom<TargetWire> for RefTarget {
    type Error = Box<dyn std::error::Error + Send + Sync>;
    fn try_from(value: TargetWire) -> Result<Self, Self::Error> {
        if value.0.len().is_multiple_of(2) {
            return Err("ref target has an even number of terms".into());
        }
        Ok(RefTarget::from_merge(Merge::from_vec(
            value
                .0
                .into_iter()
                .map(|term| term.map(CommitId::new))
                .collect::<Vec<_>>(),
        )))
    }
}

#[derive(Serialize, Deserialize)]
struct NamedTarget {
    name: String,
    target: TargetWire,
}
#[derive(Serialize, Deserialize)]
struct RemoteRefWire {
    name: String,
    target: TargetWire,
    tracked: bool,
}
#[derive(Serialize, Deserialize)]
struct RemoteViewWire {
    name: String,
    bookmarks: Vec<RemoteRefWire>,
    tags: Vec<RemoteRefWire>,
}
#[derive(Serialize, Deserialize)]
struct WorkspaceCommitWire {
    name: String,
    id: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
struct ViewWire {
    heads: Vec<Vec<u8>>,
    bookmarks: Vec<NamedTarget>,
    tags: Vec<NamedTarget>,
    remotes: Vec<RemoteViewWire>,
    git_refs: Vec<NamedTarget>,
    git_head: TargetWire,
    workspaces: Vec<WorkspaceCommitWire>,
}

pub(crate) fn encode_view(view: &View) -> Result<Vec<u8>, serde_cbor::Error> {
    serde_cbor::to_vec(&ViewWire::from(view))
}

pub(crate) fn decode_view(bytes: &[u8]) -> Result<View, Box<dyn std::error::Error + Send + Sync>> {
    View::try_from(serde_cbor::from_slice::<ViewWire>(bytes)?)
}

impl From<&View> for ViewWire {
    fn from(view: &View) -> Self {
        let named = |values: &BTreeMap<RefNameBuf, RefTarget>| {
            values
                .iter()
                .map(|(name, target)| NamedTarget {
                    name: name.as_str().to_owned(),
                    target: target.into(),
                })
                .collect()
        };
        Self {
            heads: view.head_ids.iter().map(ObjectId::to_bytes).collect(),
            bookmarks: named(&view.local_bookmarks),
            tags: named(&view.local_tags),
            remotes: view
                .remote_views
                .iter()
                .map(|(name, remote)| RemoteViewWire {
                    name: name.as_str().to_owned(),
                    bookmarks: remote
                        .bookmarks
                        .iter()
                        .map(|(name, value)| RemoteRefWire {
                            name: name.as_str().to_owned(),
                            target: (&value.target).into(),
                            tracked: value.state == RemoteRefState::Tracked,
                        })
                        .collect(),
                    tags: remote
                        .tags
                        .iter()
                        .map(|(name, value)| RemoteRefWire {
                            name: name.as_str().to_owned(),
                            target: (&value.target).into(),
                            tracked: value.state == RemoteRefState::Tracked,
                        })
                        .collect(),
                })
                .collect(),
            git_refs: view
                .git_refs
                .iter()
                .map(|(name, target)| NamedTarget {
                    name: name.as_str().to_owned(),
                    target: target.into(),
                })
                .collect(),
            git_head: (&view.git_head).into(),
            workspaces: view
                .wc_commit_ids
                .iter()
                .map(|(name, id)| WorkspaceCommitWire {
                    name: name.as_str().to_owned(),
                    id: id.to_bytes(),
                })
                .collect(),
        }
    }
}

impl TryFrom<ViewWire> for View {
    type Error = Box<dyn std::error::Error + Send + Sync>;
    fn try_from(wire: ViewWire) -> Result<Self, Self::Error> {
        let targets =
            |values: Vec<NamedTarget>| -> Result<BTreeMap<RefNameBuf, RefTarget>, Self::Error> {
                values
                    .into_iter()
                    .map(|value| Ok((RefNameBuf::from(value.name), value.target.try_into()?)))
                    .collect()
            };
        let mut remote_views = BTreeMap::new();
        for remote in wire.remotes {
            let refs = |values: Vec<RemoteRefWire>| -> Result<BTreeMap<RefNameBuf, RemoteRef>, Self::Error> {
                values.into_iter().map(|value| Ok((RefNameBuf::from(value.name), RemoteRef { target: value.target.try_into()?, state: if value.tracked { RemoteRefState::Tracked } else { RemoteRefState::New } }))).collect()
            };
            remote_views.insert(
                RemoteNameBuf::from(remote.name),
                RemoteView {
                    bookmarks: refs(remote.bookmarks)?,
                    tags: refs(remote.tags)?,
                },
            );
        }
        Ok(View {
            head_ids: wire.heads.into_iter().map(CommitId::new).collect(),
            local_bookmarks: targets(wire.bookmarks)?,
            local_tags: targets(wire.tags)?,
            remote_views,
            git_refs: wire
                .git_refs
                .into_iter()
                .map(|value| Ok((GitRefNameBuf::from(value.name), value.target.try_into()?)))
                .collect::<Result<_, Self::Error>>()?,
            git_head: wire.git_head.try_into()?,
            wc_commit_ids: wire
                .workspaces
                .into_iter()
                .map(|value| (WorkspaceNameBuf::from(value.name), CommitId::new(value.id)))
                .collect(),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct PredecessorsWire {
    commit: Vec<u8>,
    predecessors: Vec<Vec<u8>>,
}
#[derive(Serialize, Deserialize)]
struct OperationWire {
    view: Vec<u8>,
    parents: Vec<Vec<u8>>,
    start_ms: i64,
    start_tz: i32,
    end_ms: i64,
    end_tz: i32,
    description: String,
    hostname: String,
    username: String,
    snapshot: bool,
    workspace: Option<String>,
    attributes: BTreeMap<String, String>,
    predecessors: Option<Vec<PredecessorsWire>>,
}

impl From<&Operation> for OperationWire {
    fn from(value: &Operation) -> Self {
        Self {
            view: value.view_id.to_bytes(),
            parents: value.parents.iter().map(ObjectId::to_bytes).collect(),
            start_ms: value.metadata.time.start.timestamp.0,
            start_tz: value.metadata.time.start.tz_offset,
            end_ms: value.metadata.time.end.timestamp.0,
            end_tz: value.metadata.time.end.tz_offset,
            description: value.metadata.description.clone(),
            hostname: value.metadata.hostname.clone(),
            username: value.metadata.username.clone(),
            snapshot: value.metadata.is_snapshot,
            workspace: value
                .metadata
                .workspace_name
                .as_ref()
                .map(|name| name.as_str().to_owned()),
            attributes: value.metadata.attributes.clone(),
            predecessors: value.commit_predecessors.as_ref().map(|values| {
                values
                    .iter()
                    .map(|(commit, predecessors)| PredecessorsWire {
                        commit: commit.to_bytes(),
                        predecessors: predecessors.iter().map(ObjectId::to_bytes).collect(),
                    })
                    .collect()
            }),
        }
    }
}

impl From<OperationWire> for Operation {
    fn from(value: OperationWire) -> Self {
        Self {
            view_id: ViewId::new(value.view),
            parents: value.parents.into_iter().map(OperationId::new).collect(),
            metadata: OperationMetadata {
                time: TimestampRange {
                    start: Timestamp {
                        timestamp: MillisSinceEpoch(value.start_ms),
                        tz_offset: value.start_tz,
                    },
                    end: Timestamp {
                        timestamp: MillisSinceEpoch(value.end_ms),
                        tz_offset: value.end_tz,
                    },
                },
                description: value.description,
                hostname: value.hostname,
                username: value.username,
                is_snapshot: value.snapshot,
                workspace_name: value.workspace.map(WorkspaceNameBuf::from),
                attributes: value.attributes,
            },
            commit_predecessors: value.predecessors.map(|values| {
                values
                    .into_iter()
                    .map(|value| {
                        (
                            CommitId::new(value.commit),
                            value.predecessors.into_iter().map(CommitId::new).collect(),
                        )
                    })
                    .collect()
            }),
        }
    }
}
