use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use jj_lib::{
    backend::{BackendInitError, BackendLoadError, TreeValue},
    config::StackedConfig,
    default_backend_factories::{default_backend_factories, default_working_copy_factory},
    object_id::ObjectId,
    op_store::{Operation, RefTarget},
    ref_name::WorkspaceName,
    repo::{ReadonlyRepo, Repo, RepoLoader, StoreFactories},
    repo_path::RepoPathBuf,
    settings::UserSettings,
    signing::Signer,
    workspace::Workspace,
};
use serde::{Deserialize, Serialize};

use crate::{
    JJ_REPOSITORY_FORMAT_VERSION, RepositoryId, RepositorySnapshotId, UserId,
    atlas_backend::{AtlasBackend, RepositoryObjectStore, RpcRepositoryObjectStore},
    atlas_op_heads_store::AtlasOpHeadsStore,
    atlas_op_store::{
        AtlasOpStore, CheckoutObjectStore, RpcCheckoutObjectStore, decode_view, encode_view,
    },
    repository::{CheckoutId, JujutsuSnapshot, ObjectKind, RepositoryDatabase, RepositoryObjectId},
};

const CONFIG_FILE: &str = "atlas.cbor";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AtlasStoreConfig {
    repository_id: RepositoryId,
    checkout_id: CheckoutId,
    user: UserId,
    socket: PathBuf,
}

/// Initializes a native Atlas jj workspace. Commit, tree, file, operation,
/// and view objects are written through the daemon to the repository database;
/// operation heads, index, and working-copy state remain local.
pub async fn init_workspace(
    path: &Path,
    repository_id: RepositoryId,
    user: UserId,
    socket: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let objects: Arc<dyn RepositoryObjectStore> =
        Arc::new(RpcRepositoryObjectStore::new(socket.to_owned(), user));
    let checkout_objects: Arc<dyn CheckoutObjectStore> =
        Arc::new(RpcCheckoutObjectStore::new(socket.to_owned(), user));
    init_workspace_with_stores(
        path,
        repository_id,
        CheckoutId(uuid::Uuid::new_v4()),
        user,
        socket,
        objects,
        checkout_objects,
    )
    .await
}

pub async fn init_workspace_with_store(
    path: &Path,
    repository_id: RepositoryId,
    user: UserId,
    socket: &Path,
    database: Arc<RepositoryDatabase>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let objects: Arc<dyn RepositoryObjectStore> = database.clone();
    let checkout_objects: Arc<dyn CheckoutObjectStore> = database;
    init_workspace_with_stores(
        path,
        repository_id,
        CheckoutId(uuid::Uuid::new_v4()),
        user,
        socket,
        objects,
        checkout_objects,
    )
    .await
}

async fn init_workspace_with_stores(
    path: &Path,
    repository_id: RepositoryId,
    checkout_id: CheckoutId,
    user: UserId,
    socket: &Path,
    objects: Arc<dyn RepositoryObjectStore>,
    checkout_objects: Arc<dyn CheckoutObjectStore>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::fs::create_dir_all(path)?;
    checkout_objects
        .update_op_heads(repository_id, checkout_id, &[], &[0; 32])
        .await?;
    let settings = UserSettings::from_config(StackedConfig::with_defaults())?;
    let signer = Signer::from_settings(&settings)?;
    let config = serde_cbor::to_vec(&AtlasStoreConfig {
        repository_id,
        checkout_id,
        user,
        socket: socket.to_owned(),
    })?;
    let backend_objects = objects.clone();
    let backend_config = config.clone();
    let backend = move |_settings: &UserSettings, store_path: &Path| {
        std::fs::write(store_path.join(CONFIG_FILE), &backend_config)
            .map_err(|error| BackendInitError(error.into()))?;
        AtlasBackend::init(store_path, repository_id, backend_objects.clone())
            .map(|backend| Box::new(backend) as Box<dyn jj_lib::backend::Backend>)
            .map_err(BackendInitError)
    };
    let op_objects = checkout_objects;
    let heads_objects = op_objects.clone();
    let heads_config = config.clone();
    let op_store = move |_settings: &UserSettings,
                         store_path: &Path,
                         root_data: jj_lib::op_store::RootOperationData| {
        std::fs::write(store_path.join(CONFIG_FILE), &config)
            .map_err(|error| BackendInitError(error.into()))?;
        Ok(Box::new(AtlasOpStore::new(
            root_data,
            repository_id,
            checkout_id,
            op_objects.clone(),
        )) as Box<dyn jj_lib::op_store::OpStore>)
    };
    let op_heads = move |_settings: &UserSettings,
                         store_path: &Path,
                         _root_id: &jj_lib::op_store::OperationId| {
        std::fs::write(store_path.join(CONFIG_FILE), &heads_config)
            .map_err(|error| BackendInitError(error.into()))?;
        Ok(Box::new(AtlasOpHeadsStore::new(
            repository_id,
            checkout_id,
            heads_objects.clone(),
        )) as Box<dyn jj_lib::op_heads_store::OpHeadsStore>)
    };
    Workspace::init_with_factories(
        &settings,
        path,
        &backend,
        signer,
        &op_store,
        &op_heads,
        ReadonlyRepo::default_index_store_initializer(),
        ReadonlyRepo::default_submodule_store_initializer(),
        &*default_working_copy_factory(),
        WorkspaceName::DEFAULT.to_owned(),
    )
    .await?;
    Ok(())
}

pub fn additional_store_factories() -> StoreFactories {
    let mut factories = StoreFactories::empty();
    factories.add_backend(
        AtlasBackend::NAME,
        Box::new(|_settings, store_path| {
            let config: AtlasStoreConfig = serde_cbor::from_slice(
                &std::fs::read(store_path.join(CONFIG_FILE))
                    .map_err(|error| BackendLoadError(error.into()))?,
            )
            .map_err(|error| BackendLoadError(error.into()))?;
            let objects = Arc::new(RpcRepositoryObjectStore::new(config.socket, config.user));
            AtlasBackend::init(store_path, config.repository_id, objects)
                .map(|backend| Box::new(backend) as Box<dyn jj_lib::backend::Backend>)
                .map_err(BackendLoadError)
        }),
    );
    factories.add_op_store(
        AtlasOpStore::NAME,
        Box::new(|_settings, store_path, root_data| {
            let config: AtlasStoreConfig = serde_cbor::from_slice(
                &std::fs::read(store_path.join(CONFIG_FILE))
                    .map_err(|error| BackendLoadError(error.into()))?,
            )
            .map_err(|error| BackendLoadError(error.into()))?;
            let objects = Arc::new(RpcCheckoutObjectStore::new(config.socket, config.user));
            Ok(Box::new(AtlasOpStore::new(
                root_data,
                config.repository_id,
                config.checkout_id,
                objects,
            )) as Box<dyn jj_lib::op_store::OpStore>)
        }),
    );
    factories.add_op_heads_store(
        AtlasOpHeadsStore::NAME,
        Box::new(|_settings, store_path| {
            let config: AtlasStoreConfig = serde_cbor::from_slice(
                &std::fs::read(store_path.join(CONFIG_FILE))
                    .map_err(|error| BackendLoadError(error.into()))?,
            )
            .map_err(|error| BackendLoadError(error.into()))?;
            let objects = Arc::new(RpcCheckoutObjectStore::new(config.socket, config.user));
            Ok(Box::new(AtlasOpHeadsStore::new(
                config.repository_id,
                config.checkout_id,
                objects,
            ))
                as Box<dyn jj_lib::op_heads_store::OpHeadsStore>)
        }),
    );
    factories
}

pub fn store_factories() -> StoreFactories {
    let mut factories = default_backend_factories();
    factories.merge(additional_store_factories());
    factories
}

pub fn store_factories_with_store(
    repository_id: RepositoryId,
    database: Arc<RepositoryDatabase>,
) -> StoreFactories {
    let mut factories = default_backend_factories();
    let objects: Arc<dyn RepositoryObjectStore> = database.clone();
    let checkout_objects: Arc<dyn CheckoutObjectStore> = database;
    let op_head_objects = checkout_objects.clone();
    let backend_objects = objects.clone();
    factories.add_backend(
        AtlasBackend::NAME,
        Box::new(move |_settings, store_path| {
            AtlasBackend::init(store_path, repository_id, backend_objects.clone())
                .map(|backend| Box::new(backend) as Box<dyn jj_lib::backend::Backend>)
                .map_err(BackendLoadError)
        }),
    );
    factories.add_op_store(
        AtlasOpStore::NAME,
        Box::new(move |_settings, store_path, root_data| {
            let config: AtlasStoreConfig = serde_cbor::from_slice(
                &std::fs::read(store_path.join(CONFIG_FILE))
                    .map_err(|error| BackendLoadError(error.into()))?,
            )
            .map_err(|error| BackendLoadError(error.into()))?;
            Ok(Box::new(AtlasOpStore::new(
                root_data,
                repository_id,
                config.checkout_id,
                checkout_objects.clone(),
            )) as Box<dyn jj_lib::op_store::OpStore>)
        }),
    );
    factories.add_op_heads_store(
        AtlasOpHeadsStore::NAME,
        Box::new(move |_settings, store_path| {
            let config: AtlasStoreConfig = serde_cbor::from_slice(
                &std::fs::read(store_path.join(CONFIG_FILE))
                    .map_err(|error| BackendLoadError(error.into()))?,
            )
            .map_err(|error| BackendLoadError(error.into()))?;
            Ok(Box::new(AtlasOpHeadsStore::new(
                repository_id,
                config.checkout_id,
                op_head_objects.clone(),
            ))
                as Box<dyn jj_lib::op_heads_store::OpHeadsStore>)
        }),
    );
    factories
}

pub fn workspace_repository_id(
    workspace_path: &Path,
) -> Result<RepositoryId, Box<dyn std::error::Error + Send + Sync>> {
    let settings = UserSettings::from_config(StackedConfig::with_defaults())?;
    let workspace = Workspace::load(
        &settings,
        workspace_path,
        &store_factories(),
        &jj_lib::default_backend_factories::default_working_copy_factories(),
    )?;
    let config: AtlasStoreConfig = serde_cbor::from_slice(&std::fs::read(
        workspace.repo_path().join("store").join(CONFIG_FILE),
    )?)?;
    Ok(config.repository_id)
}

/// Creates an immutable, workspace-neutral snapshot and computes the exact jj
/// object closure needed to materialize it. The synthetic operation is rooted
/// directly at jj's virtual root operation so local working-copy history never
/// leaks into the shared operation graph.
pub async fn create_snapshot(
    workspace_path: &Path,
) -> Result<JujutsuSnapshot, Box<dyn std::error::Error + Send + Sync>> {
    create_snapshot_with_parents(workspace_path, BTreeSet::new()).await
}

pub async fn create_snapshot_with_parents(
    workspace_path: &Path,
    parents: BTreeSet<RepositorySnapshotId>,
) -> Result<JujutsuSnapshot, Box<dyn std::error::Error + Send + Sync>> {
    create_snapshot_with_factories_and_parents(workspace_path, &store_factories(), parents).await
}

pub async fn create_snapshot_with_factories(
    workspace_path: &Path,
    factories: &StoreFactories,
) -> Result<JujutsuSnapshot, Box<dyn std::error::Error + Send + Sync>> {
    create_snapshot_with_factories_and_parents(workspace_path, factories, BTreeSet::new()).await
}

pub async fn create_snapshot_with_factories_and_parents(
    workspace_path: &Path,
    factories: &StoreFactories,
    parents: BTreeSet<RepositorySnapshotId>,
) -> Result<JujutsuSnapshot, Box<dyn std::error::Error + Send + Sync>> {
    let settings = UserSettings::from_config(StackedConfig::with_defaults())?;
    let workspace = Workspace::load(
        &settings,
        workspace_path,
        factories,
        &jj_lib::default_backend_factories::default_working_copy_factories(),
    )?;
    let repo = workspace.repo_loader().load_at_head().await?;
    let mut shared_view = repo.view().store_view().clone();
    shared_view.wc_commit_ids.clear();
    shared_view.git_head = RefTarget::absent();

    let mut objects = BTreeSet::new();
    let store = repo.store().clone();
    let mut pending_commits: Vec<_> = shared_view.head_ids.iter().cloned().collect();
    let mut seen_commits = HashSet::new();
    let mut pending_trees = Vec::new();
    while let Some(id) = pending_commits.pop() {
        if id == *store.root_commit_id() || !seen_commits.insert(id.clone()) {
            continue;
        }
        objects.insert(RepositoryObjectId {
            kind: ObjectKind::Commit,
            id: id.as_bytes().to_vec(),
        });
        let commit = store.get_commit_async(&id).await?;
        pending_commits.extend(commit.parent_ids().iter().cloned());
        pending_trees.extend(
            commit
                .store_commit()
                .root_tree
                .iter()
                .cloned()
                .map(|tree_id| (RepoPathBuf::root(), tree_id)),
        );
    }

    let mut seen_trees = HashSet::new();
    while let Some((directory, id)) = pending_trees.pop() {
        if id == *store.empty_tree_id() || !seen_trees.insert(id.clone()) {
            continue;
        }
        objects.insert(RepositoryObjectId {
            kind: ObjectKind::Tree,
            id: id.as_bytes().to_vec(),
        });
        let tree = store.get_tree(directory.clone(), &id).await?;
        for entry in tree.data().entries() {
            match entry.value() {
                TreeValue::File { id, .. } => {
                    objects.insert(RepositoryObjectId {
                        kind: ObjectKind::File,
                        id: id.as_bytes().to_vec(),
                    });
                }
                TreeValue::Symlink(id) => {
                    objects.insert(RepositoryObjectId {
                        kind: ObjectKind::Symlink,
                        id: id.as_bytes().to_vec(),
                    });
                }
                TreeValue::Tree(id) => {
                    pending_trees.push((directory.join(entry.name()), id.clone()));
                }
                TreeValue::GitSubmodule(_) => {
                    return Err(
                        "Git submodules are unsupported by native Atlas repositories".into(),
                    );
                }
            }
        }
    }

    Ok(JujutsuSnapshot {
        format_version: JJ_REPOSITORY_FORMAT_VERSION,
        parents,
        view: encode_view(&shared_view)?,
        objects,
    })
}

pub async fn checkout_workspace(
    workspace_path: &Path,
    repository_id: RepositoryId,
    user: UserId,
    socket: &Path,
    snapshots: &[JujutsuSnapshot],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let objects: Arc<dyn RepositoryObjectStore> =
        Arc::new(RpcRepositoryObjectStore::new(socket.to_owned(), user));
    let checkout_objects: Arc<dyn CheckoutObjectStore> =
        Arc::new(RpcCheckoutObjectStore::new(socket.to_owned(), user));
    checkout_workspace_with_stores(
        workspace_path,
        repository_id,
        user,
        socket,
        snapshots,
        objects,
        checkout_objects,
        None,
    )
    .await
}

pub async fn checkout_workspace_with_store(
    workspace_path: &Path,
    repository_id: RepositoryId,
    user: UserId,
    socket: &Path,
    snapshots: &[JujutsuSnapshot],
    database: Arc<RepositoryDatabase>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let objects: Arc<dyn RepositoryObjectStore> = database.clone();
    let checkout_objects: Arc<dyn CheckoutObjectStore> = database.clone();
    checkout_workspace_with_stores(
        workspace_path,
        repository_id,
        user,
        socket,
        snapshots,
        objects,
        checkout_objects,
        Some(database),
    )
    .await
}

async fn checkout_workspace_with_stores(
    workspace_path: &Path,
    repository_id: RepositoryId,
    user: UserId,
    socket: &Path,
    snapshots: &[JujutsuSnapshot],
    objects: Arc<dyn RepositoryObjectStore>,
    checkout_objects: Arc<dyn CheckoutObjectStore>,
    database: Option<Arc<RepositoryDatabase>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if snapshots.is_empty() {
        return init_workspace_with_stores(
            workspace_path,
            repository_id,
            CheckoutId(uuid::Uuid::new_v4()),
            user,
            socket,
            objects,
            checkout_objects,
        )
        .await;
    }
    let checkout_id = CheckoutId(uuid::Uuid::new_v4());
    init_workspace_with_stores(
        workspace_path,
        repository_id,
        checkout_id,
        user,
        socket,
        objects,
        checkout_objects,
    )
    .await?;
    let settings = UserSettings::from_config(StackedConfig::with_defaults())?;
    let factories = if let Some(database) = database {
        store_factories_with_store(repository_id, database)
    } else {
        store_factories()
    };
    let workspace = Workspace::load(
        &settings,
        workspace_path,
        &factories,
        &jj_lib::default_backend_factories::default_working_copy_factories(),
    )?;
    import_snapshot_views(workspace.repo_loader(), snapshots).await?;
    workspace.repo_loader().load_at_head().await?;
    Ok(())
}

pub async fn pull_snapshots(
    workspace_path: &Path,
    snapshots: &[JujutsuSnapshot],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let factories = store_factories();
    pull_snapshots_with_factories(workspace_path, snapshots, &factories).await
}

pub async fn pull_snapshots_with_factories(
    workspace_path: &Path,
    snapshots: &[JujutsuSnapshot],
    factories: &StoreFactories,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if snapshots.is_empty() {
        return Ok(());
    }
    let settings = UserSettings::from_config(StackedConfig::with_defaults())?;
    let workspace = Workspace::load(
        &settings,
        workspace_path,
        factories,
        &jj_lib::default_backend_factories::default_working_copy_factories(),
    )?;
    import_snapshot_views(workspace.repo_loader(), snapshots).await?;
    workspace.repo_loader().load_at_head().await?;
    Ok(())
}

async fn import_snapshot_views(
    loader: &RepoLoader,
    snapshots: &[JujutsuSnapshot],
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let root = loader.root_operation().await;
    let op_store = root.op_store();
    for snapshot in snapshots {
        let view_id = op_store.write_view(&decode_view(&snapshot.view)?).await?;
        let mut metadata = root.metadata().clone();
        metadata.description = "import Atlas repository snapshot".into();
        metadata.workspace_name = None;
        metadata.is_snapshot = false;
        let operation_id = op_store
            .write_operation(&Operation {
                view_id,
                parents: vec![op_store.root_operation_id().clone()],
                metadata,
                commit_predecessors: None,
            })
            .await?;
        loader
            .op_heads_store()
            .update_op_heads(&[], &operation_id)
            .await?;
    }
    Ok(())
}
