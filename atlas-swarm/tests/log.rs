use std::{collections::BTreeSet, sync::Arc, time::Duration};

use atlas_swarm::{
    Commit, MembershipOperation, NodeCoordinate, NodeRecord, PathAcl, PathOperation,
    RepositoryRecord, ServiceRecord, SignedPathOperation, SignedUserMetadata, Store,
    SwarmOperation, SwarmPath, UserId, UserMetadata,
};
use ed25519_dalek::SigningKey;
use iroh::SecretKey;

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
    let view = store.view().await.unwrap().membership;
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
    let node = SecretKey::generate();
    let user = SigningKey::from_bytes(&[7; 32]);
    let path = SwarmPath::new("agents/echo").unwrap();
    let metadata = SignedUserMetadata::new(
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
    let root = Commit::new(
        [user_commit.id].into_iter().collect(),
        node.public(),
        SwarmOperation::InitializePathTree(SignedPathOperation::new(
            PathOperation::SetAcl {
                path: None,
                acl: PathAcl {
                    readers: [UserId::from_signing_key(&user)].into_iter().collect(),
                    writers: [UserId::from_signing_key(&user)].into_iter().collect(),
                },
            },
            &user,
        )),
        &node,
    );
    let service = Commit::new(
        [root.id].into_iter().collect(),
        node.public(),
        SwarmOperation::Path(SignedPathOperation::new(
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
            PathOperation::RemoveResource { path: path.clone() },
            &user,
        )),
        &node,
    );

    let store = atlas_swarm::MemoryStore::default();
    store
        .merge(vec![user_commit.clone(), root.clone(), service.clone()])
        .await
        .unwrap();
    let advertised = store.view().await.unwrap();
    assert_eq!(
        advertised.users[&UserId::from_signing_key(&user)]
            .username
            .as_deref(),
        Some("ada")
    );
    assert!(advertised.paths[&path].resource.is_some());
    let removed_store = atlas_swarm::MemoryStore::default();
    removed_store
        .merge(vec![user_commit, root, service, removed])
        .await
        .unwrap();
    assert!(removed_store.view().await.unwrap().paths[&path]
        .resource
        .is_none());
}

#[tokio::test(flavor = "current_thread")]
async fn repositories_and_services_share_paths_but_cannot_replace_each_other() {
    let node = SecretKey::generate();
    let user = SigningKey::from_bytes(&[9; 32]);
    let user_id = UserId::from_signing_key(&user);
    let path = SwarmPath::new("projects/atlas").unwrap();
    let root = Commit::new(
        BTreeSet::new(),
        node.public(),
        SwarmOperation::InitializePathTree(SignedPathOperation::new(
            PathOperation::SetAcl {
                path: None,
                acl: PathAcl {
                    readers: [user_id].into_iter().collect(),
                    writers: [user_id].into_iter().collect(),
                },
            },
            &user,
        )),
        &node,
    );
    let repository = Commit::new(
        [root.id].into_iter().collect(),
        node.public(),
        SwarmOperation::Path(SignedPathOperation::new(
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
    store.merge(vec![root, repository, service]).await.unwrap();
    assert!(matches!(
        store.view().await.unwrap().paths[&path].resource,
        Some(atlas_swarm::PathResource::Repository(_))
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
        .view()
        .await
        .unwrap()
        .membership
        .nodes
        .contains_key("desktop"));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires UDP socket binding, which is unavailable in the sandbox"]
async fn allowlisted_user_can_open_an_rpc_service() {
    let owner = SigningKey::from_bytes(&[12; 32]);
    let user = SigningKey::from_bytes(&[11; 32]);
    let swarm = atlas_swarm::Swarm::create(
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
        &owner,
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
