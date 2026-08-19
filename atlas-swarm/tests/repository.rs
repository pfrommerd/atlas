use std::{collections::BTreeSet, sync::Arc};

use atlas_swarm::{
    Commit, JJ_REPOSITORY_FORMAT_VERSION, MemoryStore, NodeRecord, PathAcl, PathOperation,
    RepositoryKind, RepositoryRecord, RepositorySnapshotId, Swarm, SwarmOperation, SwarmPath,
    UserId,
    atlas_backend::AtlasBackend,
    atlas_op_store::AtlasOpStore,
    local::{LocalDaemon, RepositoryObjectRequest, SwarmControl},
    native_jj,
    repository::{JujutsuSnapshot, ObjectKind, RepositoryDatabase},
};
use ed25519_dalek::SigningKey;
use futures_util::{AsyncReadExt, io::Cursor};
use jj_lib::{
    backend::Backend,
    config::StackedConfig,
    default_backend_factories::default_working_copy_factories,
    op_store::{OpStore, Operation, RootOperationData, View},
    settings::UserSettings,
    workspace::Workspace,
};

#[tokio::test]
async fn native_workspace_is_usable_by_jj_lib_0_44() {
    let directory = tempfile::tempdir().unwrap();
    let database =
        Arc::new(RepositoryDatabase::open(directory.path().join("repositories.redb")).unwrap());
    let repository_id = uuid::Uuid::new_v4();
    native_jj::init_workspace_with_store(
        &directory.path().join("workspace"),
        repository_id,
        UserId([7; 32]),
        &directory.path().join("swarm.sock"),
        database.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read_to_string(directory.path().join("workspace/.jj/repo/store/type")).unwrap(),
        "atlas"
    );
    assert_eq!(
        native_jj::workspace_repository_id(&directory.path().join("workspace")).unwrap(),
        repository_id
    );
    let settings = UserSettings::from_config(StackedConfig::with_defaults()).unwrap();
    let factories = native_jj::store_factories_with_store(repository_id, database.clone());
    let workspace = Workspace::load(
        &settings,
        &directory.path().join("workspace"),
        &factories,
        &default_working_copy_factories(),
    )
    .unwrap();
    workspace.repo_loader().load_at_head().await.unwrap();

    let snapshot =
        native_jj::create_snapshot_with_factories(&directory.path().join("workspace"), &factories)
            .await
            .unwrap();
    assert!(!snapshot.objects.is_empty());
    for object in &snapshot.objects {
        assert!(database.has_object(repository_id, object).unwrap());
    }
    assert!(snapshot.objects.iter().all(|object| matches!(
        object.kind,
        ObjectKind::Commit
            | ObjectKind::Tree
            | ObjectKind::File
            | ObjectKind::Symlink
            | ObjectKind::Conflict
            | ObjectKind::Copy
    )));

    native_jj::checkout_workspace_with_store(
        &directory.path().join("checkout"),
        repository_id,
        UserId([7; 32]),
        &directory.path().join("swarm.sock"),
        std::slice::from_ref(&snapshot),
        database.clone(),
    )
    .await
    .unwrap();
    let checkout_factories = native_jj::store_factories_with_store(repository_id, database.clone());
    let checkout = Workspace::load(
        &settings,
        &directory.path().join("checkout"),
        &checkout_factories,
        &default_working_copy_factories(),
    )
    .unwrap();
    checkout.repo_loader().load_at_head().await.unwrap();
    let republished = native_jj::create_snapshot_with_factories(
        &directory.path().join("checkout"),
        &checkout_factories,
    )
    .await
    .unwrap();
    assert!(
        snapshot
            .objects
            .iter()
            .all(|object| republished.objects.contains(object))
    );
}

#[test]
fn repository_database_is_content_addressed() {
    let directory = tempfile::tempdir().unwrap();
    let database = RepositoryDatabase::open(directory.path().join("repositories.redb")).unwrap();
    let repository_id = uuid::Uuid::new_v4();
    let hash = database
        .put_object(repository_id, ObjectKind::File, b"atlas")
        .unwrap();
    assert_eq!(
        database
            .get_object(repository_id, ObjectKind::File, hash)
            .unwrap(),
        Some(b"atlas".to_vec())
    );
    let snapshot = JujutsuSnapshot {
        format_version: JJ_REPOSITORY_FORMAT_VERSION,
        parents: BTreeSet::new(),
        view: vec![1, 2, 3],
        objects: BTreeSet::new(),
    };
    let first = database.write_snapshot(repository_id, &snapshot).unwrap();
    let second = database.write_snapshot(repository_id, &snapshot).unwrap();
    assert_eq!(first, second);
    assert_ne!(first, RepositorySnapshotId([0; 32]));
    assert_eq!(
        database.read_snapshot(repository_id, &first).unwrap(),
        Some(snapshot)
    );
    assert!(
        database
            .put_object_with_id(repository_id, ObjectKind::File, &hash, b"different")
            .is_err()
    );
}

#[tokio::test]
async fn atlas_backend_rehydrates_from_repository_database() {
    let directory = tempfile::tempdir().unwrap();
    let database =
        Arc::new(RepositoryDatabase::open(directory.path().join("repositories.redb")).unwrap());
    let repository_id = uuid::Uuid::new_v4();
    let first = AtlasBackend::init(
        &directory.path().join("cache-1"),
        repository_id,
        database.clone(),
    )
    .unwrap();
    let mut contents = Cursor::new(b"durable atlas object".to_vec());
    let id = first
        .write_file(jj_lib::repo_path::RepoPath::root(), &mut contents)
        .await
        .unwrap();

    let second = AtlasBackend::init(
        &directory.path().join("cache-2"),
        repository_id,
        database.clone(),
    )
    .unwrap();
    let mut reader = second
        .read_file(jj_lib::repo_path::RepoPath::root(), &id)
        .await
        .unwrap();
    let mut restored = Vec::new();
    reader.read_to_end(&mut restored).await.unwrap();
    assert_eq!(restored, b"durable atlas object");

    let root_data = RootOperationData {
        root_commit_id: second.root_commit_id().clone(),
    };
    let first_ops = AtlasOpStore::init(
        &directory.path().join("ops-1"),
        root_data.clone(),
        repository_id,
        database.clone(),
    )
    .unwrap();
    let view_id = first_ops
        .write_view(&View::make_root(root_data.root_commit_id.clone()))
        .await
        .unwrap();
    let root_operation = Operation::make_root(view_id.clone());
    let operation_id = first_ops
        .write_operation(&Operation {
            view_id,
            parents: vec![first_ops.root_operation_id().clone()],
            metadata: root_operation.metadata,
            commit_predecessors: Some(Default::default()),
        })
        .await
        .unwrap();
    let second_ops = AtlasOpStore::init(
        &directory.path().join("ops-1"),
        root_data,
        repository_id,
        database,
    )
    .unwrap();
    second_ops.read_operation(&operation_id).await.unwrap();
}

#[tokio::test]
async fn repository_snapshot_replicates_to_an_endpoint() {
    let directory = tempfile::tempdir().unwrap();
    let first_database = RepositoryDatabase::open(directory.path().join("first.redb")).unwrap();
    let second_database = RepositoryDatabase::open(directory.path().join("second.redb")).unwrap();
    let user_key = SigningKey::from_bytes(&[11; 32]);
    let user = UserId::from_signing_key(&user_key);
    let root_acl = PathAcl {
        readers: [user].into_iter().collect(),
        writers: [user].into_iter().collect(),
    };
    let first = Arc::new(
        Swarm::start_with_repository(
            "first",
            root_acl.clone(),
            None,
            Arc::new(MemoryStore::default()),
            Some(first_database.clone()),
        )
        .await
        .unwrap(),
    );
    let mut genesis = Commit::new_unsigned(
        BTreeSet::new(),
        first.endpoint_id(),
        user,
        1,
        SwarmOperation::Genesis {
            swarm_id: uuid::Uuid::new_v4(),
            root_acl: root_acl.clone(),
        },
    );
    genesis.sign_user(&user_key);
    first.submit_commit(genesis).await.unwrap();
    let mut first_join = Commit::new_unsigned(
        first
            .store()
            .commits()
            .await
            .unwrap()
            .into_iter()
            .map(|commit| commit.id)
            .collect(),
        first.endpoint_id(),
        user,
        2,
        SwarmOperation::Path(PathOperation::NodeJoin {
            path: SwarmPath::new("/nodes/first").unwrap(),
            node: NodeRecord {
                name: first.node_name().to_owned(),
                endpoint_id: first.endpoint_id(),
                endpoint_addr: first.endpoint_addr(),
                encryption_key: first.encryption_public_key(),
                coordinate: first.node_coordinate(),
            },
        }),
    );
    first_join.sign_user(&user_key);
    first.submit_commit(first_join).await.unwrap();
    let second = Swarm::start_with_repository(
        "second",
        root_acl,
        None,
        Arc::new(MemoryStore::default()),
        Some(second_database.clone()),
    )
    .await
    .unwrap();
    second
        .store()
        .merge(first.store().commits().await.unwrap())
        .await
        .unwrap();
    let mut second_join = Commit::new_unsigned(
        second
            .store()
            .commits()
            .await
            .unwrap()
            .into_iter()
            .map(|commit| commit.id)
            .collect(),
        second.endpoint_id(),
        user,
        3,
        SwarmOperation::Path(PathOperation::NodeJoin {
            path: SwarmPath::new("/nodes/second").unwrap(),
            node: NodeRecord {
                name: second.node_name().to_owned(),
                endpoint_id: second.endpoint_id(),
                endpoint_addr: second.endpoint_addr(),
                encryption_key: second.encryption_public_key(),
                coordinate: second.node_coordinate(),
            },
        }),
    );
    second_join.sign_user(&user_key);
    second.submit_commit(second_join).await.unwrap();
    first
        .store()
        .merge(second.store().commits().await.unwrap())
        .await
        .unwrap();

    let repository_id = uuid::Uuid::new_v4();
    let repository_path = SwarmPath::new("/repositories/code").unwrap();
    let mut define = Commit::new_unsigned(
        first
            .store()
            .commits()
            .await
            .unwrap()
            .into_iter()
            .map(|commit| commit.id)
            .collect(),
        first.endpoint_id(),
        user,
        4,
        SwarmOperation::Path(PathOperation::DefineRepository {
            path: repository_path.clone(),
            repository: RepositoryRecord {
                id: repository_id,
                kind: RepositoryKind::Jujutsu {
                    format_version: JJ_REPOSITORY_FORMAT_VERSION,
                },
                endpoints: [first.endpoint_id(), second.endpoint_id()]
                    .into_iter()
                    .collect(),
                allowed_users: [user].into_iter().collect(),
                snapshot_heads: BTreeSet::new(),
            },
        }),
    );
    define.sign_user(&user_key);
    first.submit_commit(define).await.unwrap();
    second
        .store()
        .merge(first.store().commits().await.unwrap())
        .await
        .unwrap();

    let tree = atlas_swarm::repository::RepositoryObjectId {
        kind: ObjectKind::Tree,
        id: atlas_swarm::repository::repository_object_hash(ObjectKind::Tree, b"tree").to_vec(),
    };
    let file = atlas_swarm::repository::RepositoryObjectId {
        kind: ObjectKind::File,
        id: atlas_swarm::repository::repository_object_hash(ObjectKind::File, b"file").to_vec(),
    };
    first_database
        .put_object_with_id(repository_id, tree.kind, &tree.id, b"tree")
        .unwrap();
    first_database
        .put_object_with_id(repository_id, file.kind, &file.id, b"file")
        .unwrap();
    let local = LocalDaemon::new(first.clone(), first_database.clone());
    assert!(
        SwarmControl::get_repository_object(
            &local,
            RepositoryObjectRequest {
                repository_id,
                user: UserId([99; 32]),
                object: file.clone(),
            },
        )
        .await
        .is_err()
    );
    assert_eq!(
        SwarmControl::get_repository_object(
            &local,
            RepositoryObjectRequest {
                repository_id,
                user,
                object: file.clone(),
            },
        )
        .await
        .unwrap(),
        Some(b"file".to_vec())
    );
    let snapshot = JujutsuSnapshot {
        format_version: JJ_REPOSITORY_FORMAT_VERSION,
        parents: BTreeSet::new(),
        view: vec![1],
        objects: [tree, file.clone()].into_iter().collect(),
    };
    let snapshot_id = first_database
        .write_snapshot(repository_id, &snapshot)
        .unwrap();
    let job = atlas_swarm::repository::ReplicationJob {
        repository_id,
        snapshot: snapshot_id.clone(),
        pending_endpoints: [second.endpoint_id()].into_iter().collect(),
    };
    first_database.enqueue_replication(&job).unwrap();
    first.replicate_repository_job(job);
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while second_database
            .read_snapshot(repository_id, &snapshot_id)
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        second_database
            .get_object_by_id(repository_id, &file)
            .unwrap(),
        Some(b"file".to_vec())
    );
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !first_database.replication_jobs().unwrap().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
}
