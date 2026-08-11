use std::{collections::BTreeSet, sync::Arc, time::Duration};

use atlas_swarm::local::{connect_local_service, serve_local};
use atlas_swarm::{
    Commit, MembershipOperation, NodeCoordinate, NodeRecord, PathAcl, PathOperation,
    RepositoryRecord, ServiceRecord, SignedPathOperation, SignedUserMetadata, Store,
    SwarmOperation, SwarmPath, UserId, UserMetadata,
};
use ed25519_dalek::SigningKey;
use iroh::SecretKey;
use tokio::{net::UnixListener, sync::RwLock};

#[atlas_rpc::interface]
trait Echo {
    async fn echo(&self, request: String) -> Result<String, String>;
}

#[derive(Clone)]
struct EchoService;

impl Echo for EchoService {
    async fn echo(&self, request: String) -> Result<String, String> {
        Ok(request)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn lowest_commit_id_wins_a_concurrent_name_collision() {
    let first = SecretKey::generate();
    let second = SecretKey::generate();
    let coordinate = NodeCoordinate::new(0.2, 0.8).unwrap();
    let left = Commit::new(
        BTreeSet::new(),
        first.public(),
        MembershipOperation::Join(NodeRecord {
            name: "laptop".into(),
            endpoint_id: first.public(),
            endpoint_addr: iroh::EndpointAddr::new(first.public()),
            coordinate,
        }),
        &first,
    );
    let right = Commit::new(
        BTreeSet::new(),
        second.public(),
        MembershipOperation::Join(NodeRecord {
            name: "laptop".into(),
            endpoint_id: second.public(),
            endpoint_addr: iroh::EndpointAddr::new(second.public()),
            coordinate,
        }),
        &second,
    );
    assert!(left.verify() && right.verify());
    let store = atlas_swarm::MemoryStore::default();
    store
        .merge(vec![left.clone(), right.clone()])
        .await
        .unwrap();
    let view = store.view(&PathAcl::default()).await.unwrap().membership;
    assert_eq!(
        view.nodes["laptop"].endpoint_id,
        if left.id < right.id {
            first.public()
        } else {
            second.public()
        }
    );
}

#[tokio::test(flavor = "current_thread")]
async fn signed_path_operations_materialize_the_shared_tree() {
    let swarm_id = uuid::Uuid::new_v4();
    let node = SecretKey::generate();
    let user = SigningKey::from_bytes(&[7; 32]);
    let root_acl = PathAcl {
        readers: [UserId::from_signing_key(&user)].into_iter().collect(),
        writers: [UserId::from_signing_key(&user)].into_iter().collect(),
    };
    let path = SwarmPath::new("agents/echo").unwrap();
    let metadata = SignedUserMetadata::new(
        swarm_id,
        UserMetadata {
            username: Some("ada".into()),
            real_name: Some("Ada Lovelace".into()),
        },
        &user,
    );
    let user_commit = Commit::new(
        BTreeSet::new(),
        node.public(),
        SwarmOperation::UserMetadata(metadata),
        &node,
    );
    let service = Commit::new(
        [user_commit.id].into_iter().collect(),
        node.public(),
        SwarmOperation::Path(SignedPathOperation::new(
            swarm_id,
            PathOperation::DefineService {
                path: path.clone(),
                service: ServiceRecord {
                    provider: node.public(),
                    allowed_users: [UserId::from_signing_key(&user)].into_iter().collect(),
                },
            },
            &user,
        )),
        &node,
    );
    let removed = Commit::new(
        [service.id].into_iter().collect(),
        node.public(),
        SwarmOperation::Path(SignedPathOperation::new(
            swarm_id,
            PathOperation::RemoveResource { path: path.clone() },
            &user,
        )),
        &node,
    );

    let store = atlas_swarm::MemoryStore::default();
    store
        .merge(vec![user_commit.clone(), service.clone()])
        .await
        .unwrap();
    let advertised = store.view(&root_acl).await.unwrap();
    assert_eq!(
        advertised.users[&UserId::from_signing_key(&user)]
            .username
            .as_deref(),
        Some("ada")
    );
    assert!(advertised.paths[&path].resource.is_some());
    let removed_store = atlas_swarm::MemoryStore::default();
    removed_store
        .merge(vec![user_commit, service, removed])
        .await
        .unwrap();
    assert!(removed_store.view(&root_acl).await.unwrap().paths[&path]
        .resource
        .is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn repositories_and_services_share_paths_and_can_replace_each_other() {
    let swarm_id = uuid::Uuid::new_v4();
    let node = SecretKey::generate();
    let user = SigningKey::from_bytes(&[9; 32]);
    let user_id = UserId::from_signing_key(&user);
    let path = SwarmPath::new("projects/atlas").unwrap();
    let root_acl = PathAcl {
        readers: [user_id].into_iter().collect(),
        writers: [user_id].into_iter().collect(),
    };
    let repository = Commit::new(
        BTreeSet::new(),
        node.public(),
        SwarmOperation::Path(SignedPathOperation::new(
            swarm_id,
            PathOperation::DefineRepository {
                path: path.clone(),
                repository: RepositoryRecord {
                    endpoints: BTreeSet::new(),
                    allowed_users: [user_id].into_iter().collect(),
                },
            },
            &user,
        )),
        &node,
    );
    let service = Commit::new(
        [repository.id].into_iter().collect(),
        node.public(),
        SwarmOperation::Path(SignedPathOperation::new(
            swarm_id,
            PathOperation::DefineService {
                path: path.clone(),
                service: ServiceRecord {
                    provider: node.public(),
                    allowed_users: [user_id].into_iter().collect(),
                },
            },
            &user,
        )),
        &node,
    );
    let store = atlas_swarm::MemoryStore::default();
    store.merge(vec![repository, service]).await.unwrap();
    assert!(matches!(
        store.view(&root_acl).await.unwrap().paths[&path].resource,
        Some(atlas_swarm::PathResource::Service(_))
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn store_creates_local_commits_from_its_current_head() {
    let key = SecretKey::generate();
    let store = atlas_swarm::MemoryStore::default();
    let coordinate = NodeCoordinate::new(0.2, 0.8).unwrap();
    let joined = store
        .append_operation(
            key.public(),
            MembershipOperation::Join(NodeRecord {
                name: "laptop".into(),
                endpoint_id: key.public(),
                endpoint_addr: iroh::EndpointAddr::new(key.public()),
                coordinate,
            })
            .into(),
            &key,
        )
        .await
        .unwrap();
    let renamed = store
        .append_operation(
            key.public(),
            MembershipOperation::Rename {
                name: "desktop".into(),
            }
            .into(),
            &key,
        )
        .await
        .unwrap();
    assert_eq!(renamed.parents, [joined.id].into_iter().collect());
    assert!(store
        .view(&PathAcl::default())
        .await
        .unwrap()
        .membership
        .nodes
        .contains_key("desktop"));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires Unix socket binding, which is unavailable in the sandbox"]
async fn local_service_transport_uses_the_same_acl_authentication() {
    let user = SigningKey::from_bytes(&[13; 32]);
    let user_id = UserId::from_signing_key(&user);
    let path = SwarmPath::new("local/echo").unwrap();
    let view = atlas_swarm::SwarmView {
        membership: Default::default(),
        users: Default::default(),
        root_acl: Some(PathAcl {
            readers: [user_id].into_iter().collect(),
            writers: Default::default(),
        }),
        paths: [(
            path.clone(),
            atlas_swarm::PathEntry {
                acl: None,
                resource: Some(atlas_swarm::PathResource::Service(ServiceRecord {
                    provider: SecretKey::generate().public(),
                    allowed_users: [user_id].into_iter().collect(),
                })),
            },
        )]
        .into_iter()
        .collect(),
    };
    let socket =
        std::env::temp_dir().join(format!("atlas-swarm-local-{}.sock", uuid::Uuid::new_v4()));
    let listener = UnixListener::bind(&socket).unwrap();
    let state = atlas_swarm::local::PathState {
        path: path.clone(),
        entry: view.paths.get(&path).cloned(),
        effective_acl: view.root_acl.clone(),
    };
    let task = tokio::spawn(serve_local::<EchoHandle, _>(
        listener,
        path.clone(),
        Arc::new(RwLock::new(state)),
        EchoService,
    ));
    let peer = connect_local_service(&socket, &path, &user).await.unwrap();
    let handle = EchoHandle::new(peer);
    assert_eq!(handle.echo("local".into()).await.unwrap(), "local");
    task.abort();
    let _ = std::fs::remove_file(socket);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires UDP socket binding, which is unavailable in the sandbox"]
async fn allowlisted_user_can_open_an_rpc_service() {
    let owner = SigningKey::from_bytes(&[12; 32]);
    let user = SigningKey::from_bytes(&[11; 32]);
    let swarm = atlas_swarm::Swarm::start(
        "host",
        PathAcl {
            readers: [
                UserId::from_signing_key(&owner),
                UserId::from_signing_key(&user),
            ]
            .into_iter()
            .collect(),
            writers: [UserId::from_signing_key(&owner)].into_iter().collect(),
        },
        None,
        Arc::new(atlas_swarm::MemoryStore::default()),
    )
    .await
    .unwrap();
    let path = SwarmPath::new("test/echo").unwrap();
    swarm
        .serve::<EchoHandle, _>(
            &owner,
            path.clone(),
            [UserId::from_signing_key(&user)].into_iter().collect(),
            EchoService,
        )
        .await
        .unwrap();
    let peer = tokio::time::timeout(Duration::from_secs(5), swarm.service(&path, &user))
        .await
        .unwrap()
        .unwrap();
    let handle = EchoHandle::new(peer);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), handle.echo("hello".into()))
            .await
            .unwrap()
            .unwrap(),
        "hello"
    );
}
