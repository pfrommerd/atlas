use std::{
    fmt::{Debug, Formatter},
    pin::Pin,
    sync::Arc,
    time::SystemTime,
};

use async_trait::async_trait;
use futures_util::{AsyncRead, AsyncReadExt, StreamExt, io::Cursor, stream, stream::BoxStream};
use jj_lib::{
    backend::{
        Backend, BackendError, BackendResult, ChangeId, Commit, CommitId, CopyHistory, CopyId,
        CopyRecord, FileId, MillisSinceEpoch, RelatedCopy, SecureSig, Signature, SigningFn,
        SymlinkId, Timestamp, Tree, TreeId, TreeValue, make_root_commit,
    },
    conflict_labels::ConflictLabels,
    index::Index,
    merge::MergeBuilder,
    object_id::ObjectId,
    repo_path::{RepoPath, RepoPathBuf, RepoPathComponentBuf},
};
use prost::Message;
use serde::{Deserialize, Serialize};

use crate::{
    RepositoryId, UserId,
    local::{PutRepositoryObjectRequest, RepositoryObjectRequest, connect_control},
    repository::{ObjectKind, RepositoryDatabase, RepositoryObjectId, repository_object_hash},
};

const ID_LENGTH: usize = 32;
const CHANGE_ID_LENGTH: usize = 16;

#[async_trait]
pub trait RepositoryObjectStore: Send + Sync {
    async fn get(&self, object: &RepositoryObjectId) -> Result<Option<Vec<u8>>, crate::BoxError>;
    async fn put(&self, object: &RepositoryObjectId, bytes: &[u8]) -> Result<(), crate::BoxError>;
}

#[derive(Clone)]
pub struct DatabaseRepositoryObjectStore {
    database: Arc<RepositoryDatabase>,
    repository_id: RepositoryId,
}

impl DatabaseRepositoryObjectStore {
    pub fn new(database: Arc<RepositoryDatabase>, repository_id: RepositoryId) -> Self {
        Self {
            database,
            repository_id,
        }
    }
}

#[async_trait]
impl RepositoryObjectStore for DatabaseRepositoryObjectStore {
    async fn get(&self, object: &RepositoryObjectId) -> Result<Option<Vec<u8>>, crate::BoxError> {
        self.database.get_object_by_id(self.repository_id, object)
    }

    async fn put(&self, object: &RepositoryObjectId, bytes: &[u8]) -> Result<(), crate::BoxError> {
        self.database
            .put_object_by_id(self.repository_id, object, bytes)
    }
}

#[derive(Clone, Debug)]
pub struct RpcRepositoryObjectStore {
    socket: std::path::PathBuf,
    user: UserId,
    repository_id: RepositoryId,
}

impl RpcRepositoryObjectStore {
    pub fn new(socket: std::path::PathBuf, user: UserId, repository_id: RepositoryId) -> Self {
        Self {
            socket,
            user,
            repository_id,
        }
    }
}

#[async_trait]
impl RepositoryObjectStore for RpcRepositoryObjectStore {
    async fn get(&self, object: &RepositoryObjectId) -> Result<Option<Vec<u8>>, crate::BoxError> {
        connect_control(&self.socket)
            .await?
            .get_repository_object(RepositoryObjectRequest {
                repository_id: self.repository_id,
                user: self.user,
                object: object.clone(),
            })
            .await
            .map_err(|error| std::io::Error::other(error).into())
    }

    async fn put(&self, object: &RepositoryObjectId, bytes: &[u8]) -> Result<(), crate::BoxError> {
        connect_control(&self.socket)
            .await?
            .put_repository_object(PutRepositoryObjectRequest {
                repository_id: self.repository_id,
                user: self.user,
                object: object.clone(),
                bytes: bytes.to_vec(),
            })
            .await
            .map_err(|error| std::io::Error::other(error).into())
    }
}

/// A jj backend whose only durable data is in Atlas's repository database.
/// There is intentionally no filesystem object cache.
pub struct AtlasBackend {
    objects: Arc<dyn RepositoryObjectStore>,
    root_commit_id: CommitId,
    root_change_id: ChangeId,
    empty_tree_id: TreeId,
}

impl Debug for AtlasBackend {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("AtlasBackend").finish()
    }
}

impl AtlasBackend {
    pub const NAME: &'static str = "atlas";

    pub fn new(objects: Arc<dyn RepositoryObjectStore>) -> Self {
        let empty_tree = encode_tree(&Tree::default());
        Self {
            objects,
            root_commit_id: CommitId::new(vec![0; ID_LENGTH]),
            root_change_id: ChangeId::new(vec![0; CHANGE_ID_LENGTH]),
            empty_tree_id: TreeId::new(
                repository_object_hash(ObjectKind::Tree, &empty_tree).to_vec(),
            ),
        }
    }

    async fn read_object(&self, kind: ObjectKind, id: &impl ObjectId) -> BackendResult<Vec<u8>> {
        self.objects
            .get(&RepositoryObjectId {
                kind,
                id: id.as_bytes().to_vec(),
            })
            .await
            .map_err(BackendError::Other)?
            .ok_or_else(|| BackendError::ObjectNotFound {
                object_type: id.object_type(),
                hash: id.hex(),
                source: "object is absent from the Atlas repository".into(),
            })
    }

    async fn write_object(&self, kind: ObjectKind, bytes: &[u8]) -> BackendResult<Vec<u8>> {
        let id = repository_object_hash(kind, bytes).to_vec();
        self.objects
            .put(
                &RepositoryObjectId {
                    kind,
                    id: id.clone(),
                },
                bytes,
            )
            .await
            .map_err(BackendError::Other)?;
        Ok(id)
    }
}

#[async_trait]
impl Backend for AtlasBackend {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn commit_id_length(&self) -> usize {
        ID_LENGTH
    }
    fn change_id_length(&self) -> usize {
        CHANGE_ID_LENGTH
    }
    fn root_commit_id(&self) -> &CommitId {
        &self.root_commit_id
    }
    fn root_change_id(&self) -> &ChangeId {
        &self.root_change_id
    }
    fn empty_tree_id(&self) -> &TreeId {
        &self.empty_tree_id
    }
    fn concurrency(&self) -> usize {
        64
    }

    async fn read_file(
        &self,
        _path: &RepoPath,
        id: &FileId,
    ) -> BackendResult<Pin<Box<dyn AsyncRead + Send>>> {
        Ok(Box::pin(Cursor::new(
            self.read_object(ObjectKind::File, id).await?,
        )))
    }

    async fn write_file(
        &self,
        _path: &RepoPath,
        contents: &mut (dyn AsyncRead + Send + Unpin),
    ) -> BackendResult<FileId> {
        let mut bytes = Vec::new();
        contents
            .read_to_end(&mut bytes)
            .await
            .map_err(|error| BackendError::Other(error.into()))?;
        Ok(FileId::new(
            self.write_object(ObjectKind::File, &bytes).await?,
        ))
    }

    async fn read_symlink(&self, _path: &RepoPath, id: &SymlinkId) -> BackendResult<String> {
        String::from_utf8(self.read_object(ObjectKind::Symlink, id).await?)
            .map_err(|error| BackendError::Other(error.into()))
    }

    async fn write_symlink(&self, _path: &RepoPath, target: &str) -> BackendResult<SymlinkId> {
        Ok(SymlinkId::new(
            self.write_object(ObjectKind::Symlink, target.as_bytes())
                .await?,
        ))
    }

    async fn read_copy(&self, id: &CopyId) -> BackendResult<CopyHistory> {
        let wire: CopyWire = serde_cbor::from_slice(&self.read_object(ObjectKind::Copy, id).await?)
            .map_err(|error| BackendError::Other(error.into()))?;
        Ok(CopyHistory {
            current_path: RepoPathBuf::from_internal_string(wire.current_path)
                .map_err(|error| BackendError::Other(error.into()))?,
            parents: wire.parents.into_iter().map(CopyId::new).collect(),
            salt: wire.salt,
        })
    }

    async fn write_copy(&self, copy: &CopyHistory) -> BackendResult<CopyId> {
        let bytes = serde_cbor::to_vec(&CopyWire {
            current_path: copy.current_path.as_internal_file_string().to_owned(),
            parents: copy.parents.iter().map(ObjectId::to_bytes).collect(),
            salt: copy.salt.clone(),
        })
        .map_err(|error| BackendError::Other(error.into()))?;
        Ok(CopyId::new(
            self.write_object(ObjectKind::Copy, &bytes).await?,
        ))
    }

    async fn get_related_copies(&self, copy_id: &CopyId) -> BackendResult<Vec<RelatedCopy>> {
        Ok(vec![RelatedCopy {
            id: copy_id.clone(),
            history: self.read_copy(copy_id).await?,
        }])
    }

    async fn read_tree(&self, _path: &RepoPath, id: &TreeId) -> BackendResult<Tree> {
        if id == self.empty_tree_id() {
            return Ok(Tree::default());
        }
        decode_tree(&self.read_object(ObjectKind::Tree, id).await?)
    }

    async fn write_tree(&self, _path: &RepoPath, tree: &Tree) -> BackendResult<TreeId> {
        Ok(TreeId::new(
            self.write_object(ObjectKind::Tree, &encode_tree(tree))
                .await?,
        ))
    }

    async fn read_commit(&self, id: &CommitId) -> BackendResult<Commit> {
        if id == self.root_commit_id() {
            return Ok(make_root_commit(
                self.root_change_id.clone(),
                self.empty_tree_id.clone(),
            ));
        }
        decode_commit(&self.read_object(ObjectKind::Commit, id).await?)
    }

    async fn write_commit(
        &self,
        mut commit: Commit,
        sign_with: Option<&mut SigningFn>,
    ) -> BackendResult<(CommitId, Commit)> {
        if commit.parents.is_empty() {
            return Err(BackendError::Other(
                "cannot write a non-root commit without parents".into(),
            ));
        }
        let mut proto = commit_to_proto(&commit);
        if let Some(sign) = sign_with {
            let data = proto.encode_to_vec();
            let sig = sign(&data).map_err(|error| BackendError::Other(error.into()))?;
            proto.secure_sig = Some(sig.clone());
            commit.secure_sig = Some(SecureSig { data, sig });
        }
        let id = CommitId::new(
            self.write_object(ObjectKind::Commit, &proto.encode_to_vec())
                .await?,
        );
        Ok((id, commit))
    }

    fn get_copy_records(
        &self,
        _paths: Option<&[RepoPathBuf]>,
        _root: &CommitId,
        _head: &CommitId,
    ) -> BackendResult<BoxStream<'_, BackendResult<CopyRecord>>> {
        Ok(stream::empty().boxed())
    }

    fn gc(&self, _index: &dyn Index, _keep_newer: SystemTime) -> BackendResult<()> {
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct CopyWire {
    current_path: String,
    parents: Vec<Vec<u8>>,
    salt: Vec<u8>,
}

fn encode_tree(tree: &Tree) -> Vec<u8> {
    tree_to_proto(tree).encode_to_vec()
}

fn decode_tree(bytes: &[u8]) -> BackendResult<Tree> {
    let proto = jj_lib::protos::simple_store::Tree::decode(bytes)
        .map_err(|error| BackendError::Other(error.into()))?;
    let entries = proto
        .entries
        .into_iter()
        .map(|entry| {
            let name = RepoPathComponentBuf::new(entry.name)
                .map_err(|error| BackendError::Other(error.into()))?;
            let value = match entry
                .value
                .and_then(|value| value.value)
                .ok_or_else(|| BackendError::Other("tree entry is missing a value".into()))?
            {
                jj_lib::protos::simple_store::tree_value::Value::TreeId(id) => {
                    TreeValue::Tree(TreeId::new(id))
                }
                jj_lib::protos::simple_store::tree_value::Value::SymlinkId(id) => {
                    TreeValue::Symlink(SymlinkId::new(id))
                }
                jj_lib::protos::simple_store::tree_value::Value::File(file) => TreeValue::File {
                    id: FileId::new(file.id),
                    executable: file.executable,
                    copy_id: CopyId::new(file.copy_id),
                },
            };
            Ok((name, value))
        })
        .collect::<BackendResult<Vec<_>>>()?;
    Ok(Tree::from_sorted_entries(entries))
}

fn tree_to_proto(tree: &Tree) -> jj_lib::protos::simple_store::Tree {
    let entries = tree
        .entries()
        .map(|entry| {
            use jj_lib::protos::simple_store::{TreeValue as Wire, tree, tree_value};
            let value = match entry.value() {
                TreeValue::File {
                    id,
                    executable,
                    copy_id,
                } => tree_value::Value::File(tree_value::File {
                    id: id.to_bytes(),
                    executable: *executable,
                    copy_id: copy_id.to_bytes(),
                }),
                TreeValue::Symlink(id) => tree_value::Value::SymlinkId(id.to_bytes()),
                TreeValue::Tree(id) => tree_value::Value::TreeId(id.to_bytes()),
                TreeValue::GitSubmodule(_) => panic!("Git submodules are unsupported by Atlas"),
            };
            tree::Entry {
                name: entry.name().as_internal_str().to_owned(),
                value: Some(Wire { value: Some(value) }),
            }
        })
        .collect();
    jj_lib::protos::simple_store::Tree { entries }
}

fn decode_commit(bytes: &[u8]) -> BackendResult<Commit> {
    let mut proto = jj_lib::protos::simple_store::Commit::decode(bytes)
        .map_err(|error| BackendError::Other(error.into()))?;
    let secure_sig = proto.secure_sig.take().map(|sig| SecureSig {
        data: proto.encode_to_vec(),
        sig,
    });
    let root_tree: MergeBuilder<_> = proto.root_tree.into_iter().map(TreeId::new).collect();
    let conflict_labels = ConflictLabels::from_vec(proto.conflict_labels);
    Ok(Commit {
        parents: proto.parents.into_iter().map(CommitId::new).collect(),
        predecessors: proto.predecessors.into_iter().map(CommitId::new).collect(),
        root_tree: root_tree.build(),
        conflict_labels: conflict_labels.into_merge(),
        change_id: ChangeId::new(proto.change_id),
        description: proto.description,
        author: signature_from_proto(proto.author.unwrap_or_default()),
        committer: signature_from_proto(proto.committer.unwrap_or_default()),
        secure_sig,
    })
}

fn commit_to_proto(commit: &Commit) -> jj_lib::protos::simple_store::Commit {
    jj_lib::protos::simple_store::Commit {
        parents: commit.parents.iter().map(ObjectId::to_bytes).collect(),
        predecessors: commit.predecessors.iter().map(ObjectId::to_bytes).collect(),
        root_tree: commit.root_tree.iter().map(ObjectId::to_bytes).collect(),
        conflict_labels: if commit.conflict_labels.is_resolved() {
            vec![]
        } else {
            commit.conflict_labels.as_slice().to_owned()
        },
        change_id: commit.change_id.to_bytes(),
        description: commit.description.clone(),
        author: Some(signature_to_proto(&commit.author)),
        committer: Some(signature_to_proto(&commit.committer)),
        secure_sig: None,
    }
}

fn signature_to_proto(value: &Signature) -> jj_lib::protos::simple_store::commit::Signature {
    jj_lib::protos::simple_store::commit::Signature {
        name: value.name.clone(),
        email: value.email.clone(),
        timestamp: Some(jj_lib::protos::simple_store::commit::Timestamp {
            millis_since_epoch: value.timestamp.timestamp.0,
            tz_offset: value.timestamp.tz_offset,
        }),
    }
}

fn signature_from_proto(value: jj_lib::protos::simple_store::commit::Signature) -> Signature {
    let timestamp = value.timestamp.unwrap_or_default();
    Signature {
        name: value.name,
        email: value.email,
        timestamp: Timestamp {
            timestamp: MillisSinceEpoch(timestamp.millis_since_epoch),
            tz_offset: timestamp.tz_offset,
        },
    }
}
