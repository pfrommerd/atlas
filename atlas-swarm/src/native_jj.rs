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
    atlas_backend::{
        AtlasBackend, DatabaseRepositoryObjectStore, RepositoryObjectStore,
        RpcRepositoryObjectStore,
    },
    atlas_op_heads_store::AtlasOpHeadsStore,
    atlas_op_store::{
        AtlasOpStore, CheckoutStore, DatabaseCheckoutStore, RpcCheckoutStore, decode_view,
        encode_view,
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

struct AtlasStores {
    repository: Arc<dyn RepositoryObjectStore>,
    checkout: Arc<dyn CheckoutStore>,
}

fn read_store_config(store_path: &Path) -> Result<AtlasStoreConfig, BackendLoadError> {
    let bytes = std::fs::read(store_path.join(CONFIG_FILE))
        .map_err(|error| BackendLoadError(error.into()))?;
    serde_cbor::from_slice(&bytes).map_err(|error| BackendLoadError(error.into()))
}

fn rpc_stores(config: &AtlasStoreConfig) -> AtlasStores {
    AtlasStores {
        repository: Arc::new(RpcRepositoryObjectStore::new(
            config.socket.clone(),
            config.user,
            config.repository_id,
        )),
        checkout: Arc::new(RpcCheckoutStore::new(
            config.socket.clone(),
            config.user,
            config.repository_id,
            config.checkout_id,
        )),
    }
}

fn database_stores(
    database: Arc<RepositoryDatabase>,
    repository_id: RepositoryId,
    checkout_id: CheckoutId,
) -> AtlasStores {
    AtlasStores {
        repository: Arc::new(DatabaseRepositoryObjectStore::new(
            database.clone(),
            repository_id,
        )),
        checkout: Arc::new(DatabaseCheckoutStore::new(
            database,
            repository_id,
            checkout_id,
        )),
    }
}

/// Initializes a native Atlas jj workspace. Commit, tree, file, operation,
/// and view objects are written through the daemon to the repository database;
/// operation heads, index, and working-copy state remain local.
pub async fn init_workspace(
    path: &Path,
    repository_id: RepositoryId,
    user: UserId,
    socket: &Path,
) -> Result<(), crate::BoxError> {
    let config = AtlasStoreConfig {
        repository_id,
        checkout_id: CheckoutId(uuid::Uuid::new_v4()),
        user,
        socket: socket.to_owned(),
    };
    let stores = rpc_stores(&config);
    init_workspace_with_stores(path, &config, stores).await
}

pub async fn init_workspace_with_store(
    path: &Path,
    repository_id: RepositoryId,
    user: UserId,
    socket: &Path,
    database: Arc<RepositoryDatabase>,
) -> Result<(), crate::BoxError> {
    let config = AtlasStoreConfig {
        repository_id,
        checkout_id: CheckoutId(uuid::Uuid::new_v4()),
        user,
        socket: socket.to_owned(),
    };
    let stores = database_stores(database, repository_id, config.checkout_id);
    init_workspace_with_stores(path, &config, stores).await
}

async fn init_workspace_with_stores(
    path: &Path,
    config: &AtlasStoreConfig,
    stores: AtlasStores,
) -> Result<(), crate::BoxError> {
    std::fs::create_dir_all(path)?;
    stores.checkout.update_op_heads(&[], &[0; 32]).await?;
    let settings = UserSettings::from_config(StackedConfig::with_defaults())?;
    let signer = Signer::from_settings(&settings)?;
    let config_bytes = serde_cbor::to_vec(config)?;
    let backend_objects = stores.repository;
    let backend_config = config_bytes.clone();
    let backend = move |_settings: &UserSettings, store_path: &Path| {
        std::fs::write(store_path.join(CONFIG_FILE), &backend_config)
            .map_err(|error| BackendInitError(error.into()))?;
        Ok(Box::new(AtlasBackend::new(backend_objects.clone()))
            as Box<dyn jj_lib::backend::Backend>)
    };
    let op_objects = stores.checkout;
    let heads_objects = op_objects.clone();
    let heads_config = config_bytes.clone();
    let op_config = config_bytes;
    let op_store = move |_settings: &UserSettings,
                         store_path: &Path,
                         root_data: jj_lib::op_store::RootOperationData| {
        std::fs::write(store_path.join(CONFIG_FILE), &op_config)
            .map_err(|error| BackendInitError(error.into()))?;
        Ok(Box::new(AtlasOpStore::new(root_data, op_objects.clone()))
            as Box<dyn jj_lib::op_store::OpStore>)
    };
    let op_heads = move |_settings: &UserSettings,
                         store_path: &Path,
                         _root_id: &jj_lib::op_store::OperationId| {
        std::fs::write(store_path.join(CONFIG_FILE), &heads_config)
            .map_err(|error| BackendInitError(error.into()))?;
        Ok(Box::new(AtlasOpHeadsStore::new(heads_objects.clone()))
            as Box<dyn jj_lib::op_heads_store::OpHeadsStore>)
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
            let config = read_store_config(store_path)?;
            let objects = Arc::new(RpcRepositoryObjectStore::new(
                config.socket,
                config.user,
                config.repository_id,
            ));
            Ok(Box::new(AtlasBackend::new(objects)) as Box<dyn jj_lib::backend::Backend>)
        }),
    );
    factories.add_op_store(
        AtlasOpStore::NAME,
        Box::new(|_settings, store_path, root_data| {
            let config = read_store_config(store_path)?;
            let checkout = Arc::new(RpcCheckoutStore::new(
                config.socket,
                config.user,
                config.repository_id,
                config.checkout_id,
            ));
            Ok(Box::new(AtlasOpStore::new(root_data, checkout))
                as Box<dyn jj_lib::op_store::OpStore>)
        }),
    );
    factories.add_op_heads_store(
        AtlasOpHeadsStore::NAME,
        Box::new(|_settings, store_path| {
            let config = read_store_config(store_path)?;
            let checkout = Arc::new(RpcCheckoutStore::new(
                config.socket,
                config.user,
                config.repository_id,
                config.checkout_id,
            ));
            Ok(Box::new(AtlasOpHeadsStore::new(checkout))
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

pub fn store_factories_with_store(database: Arc<RepositoryDatabase>) -> StoreFactories {
    let mut factories = default_backend_factories();
    let backend_database = database.clone();
    factories.add_backend(
        AtlasBackend::NAME,
        Box::new(move |_settings, store_path| {
            let config = read_store_config(store_path)?;
            let objects = Arc::new(DatabaseRepositoryObjectStore::new(
                backend_database.clone(),
                config.repository_id,
            ));
            Ok(Box::new(AtlasBackend::new(objects)) as Box<dyn jj_lib::backend::Backend>)
        }),
    );
    let op_database = database.clone();
    factories.add_op_store(
        AtlasOpStore::NAME,
        Box::new(move |_settings, store_path, root_data| {
            let config = read_store_config(store_path)?;
            let checkout = Arc::new(DatabaseCheckoutStore::new(
                op_database.clone(),
                config.repository_id,
                config.checkout_id,
            ));
            Ok(Box::new(AtlasOpStore::new(root_data, checkout))
                as Box<dyn jj_lib::op_store::OpStore>)
        }),
    );
    let heads_database = database;
    factories.add_op_heads_store(
        AtlasOpHeadsStore::NAME,
        Box::new(move |_settings, store_path| {
            let config = read_store_config(store_path)?;
            let checkout = Arc::new(DatabaseCheckoutStore::new(
                heads_database.clone(),
                config.repository_id,
                config.checkout_id,
            ));
            Ok(Box::new(AtlasOpHeadsStore::new(checkout))
                as Box<dyn jj_lib::op_heads_store::OpHeadsStore>)
        }),
    );
    factories
}

pub fn workspace_repository_id(workspace_path: &Path) -> Result<RepositoryId, crate::BoxError> {
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
pub async fn create_snapshot_with_parents(
    workspace_path: &Path,
    parents: BTreeSet<RepositorySnapshotId>,
) -> Result<JujutsuSnapshot, crate::BoxError> {
    create_snapshot_with_factories_and_parents(workspace_path, &store_factories(), parents).await
}

pub async fn create_snapshot_with_factories(
    workspace_path: &Path,
    factories: &StoreFactories,
) -> Result<JujutsuSnapshot, crate::BoxError> {
    create_snapshot_with_factories_and_parents(workspace_path, factories, BTreeSet::new()).await
}

async fn create_snapshot_with_factories_and_parents(
    workspace_path: &Path,
    factories: &StoreFactories,
    parents: BTreeSet<RepositorySnapshotId>,
) -> Result<JujutsuSnapshot, crate::BoxError> {
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
) -> Result<(), crate::BoxError> {
    let config = AtlasStoreConfig {
        repository_id,
        checkout_id: CheckoutId(uuid::Uuid::new_v4()),
        user,
        socket: socket.to_owned(),
    };
    let stores = rpc_stores(&config);
    let factories = store_factories();
    checkout_workspace_with_stores(workspace_path, &config, snapshots, stores, &factories).await
}

pub async fn checkout_workspace_with_store(
    workspace_path: &Path,
    repository_id: RepositoryId,
    user: UserId,
    socket: &Path,
    snapshots: &[JujutsuSnapshot],
    database: Arc<RepositoryDatabase>,
) -> Result<(), crate::BoxError> {
    let config = AtlasStoreConfig {
        repository_id,
        checkout_id: CheckoutId(uuid::Uuid::new_v4()),
        user,
        socket: socket.to_owned(),
    };
    let stores = database_stores(database.clone(), repository_id, config.checkout_id);
    let factories = store_factories_with_store(database);
    checkout_workspace_with_stores(workspace_path, &config, snapshots, stores, &factories).await
}

async fn checkout_workspace_with_stores(
    workspace_path: &Path,
    config: &AtlasStoreConfig,
    snapshots: &[JujutsuSnapshot],
    stores: AtlasStores,
    factories: &StoreFactories,
) -> Result<(), crate::BoxError> {
    if snapshots.is_empty() {
        return init_workspace_with_stores(workspace_path, config, stores).await;
    }
    init_workspace_with_stores(workspace_path, config, stores).await?;
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

pub async fn pull_snapshots(
    workspace_path: &Path,
    snapshots: &[JujutsuSnapshot],
) -> Result<(), crate::BoxError> {
    let factories = store_factories();
    pull_snapshots_with_factories(workspace_path, snapshots, &factories).await
}

async fn pull_snapshots_with_factories(
    workspace_path: &Path,
    snapshots: &[JujutsuSnapshot],
    factories: &StoreFactories,
) -> Result<(), crate::BoxError> {
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
) -> Result<(), crate::BoxError> {
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
