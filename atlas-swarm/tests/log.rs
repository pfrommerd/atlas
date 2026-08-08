use std::{collections::BTreeSet, sync::Arc, time::Duration};

use atlas_swarm::{Commit, MembershipOperation, NodeCoordinate, NodeRecord, ServicePath, ServiceRecord, SignedUserMetadata, Store, SwarmOperation, UserId, UserMetadata};
use ed25519_dalek::SigningKey;
use iroh::SecretKey;

#[atlas_rpc::interface]
trait Echo {
    async fn echo(&self, request: String) -> Result<String, String>;
}

#[derive(Clone)]
struct EchoService;

impl Echo for EchoService {
    async fn echo(&self, request: String) -> Result<String, String> { Ok(request) }
}

#[tokio::test(flavor = "current_thread")]
async fn lowest_commit_id_wins_a_concurrent_name_collision() {
    let first = SecretKey::generate();
    let second = SecretKey::generate();
    let coordinate = NodeCoordinate::new(0.2, 0.8).unwrap();
    let left = Commit::new(BTreeSet::new(), first.public(), MembershipOperation::Join(NodeRecord { name: "laptop".into(), endpoint_id: first.public(), endpoint_addr: iroh::EndpointAddr::new(first.public()), coordinate }), &first);
    let right = Commit::new(BTreeSet::new(), second.public(), MembershipOperation::Join(NodeRecord { name: "laptop".into(), endpoint_id: second.public(), endpoint_addr: iroh::EndpointAddr::new(second.public()), coordinate }), &second);
    assert!(left.verify() && right.verify());
    let store = atlas_swarm::MemoryStore::default();
    store.merge(vec![left.clone(), right.clone()]).await.unwrap();
    let view = store.view().await.unwrap().membership;
    assert_eq!(view.nodes["laptop"].endpoint_id, if left.id < right.id { first.public() } else { second.public() });
}

#[tokio::test(flavor = "current_thread")]
async fn signed_user_metadata_and_service_tombstones_materialize_in_the_shared_view() {
    let node = SecretKey::generate();
    let user = SigningKey::from_bytes(&[7; 32]);
    let path = ServicePath::new("agents/echo").unwrap();
    let metadata = SignedUserMetadata::new(UserMetadata { username: Some("ada".into()), real_name: Some("Ada Lovelace".into()) }, &user);
    let user_commit = Commit::new(BTreeSet::new(), node.public(), SwarmOperation::UserMetadata(metadata), &node);
    let service = Commit::new([user_commit.id].into_iter().collect(), node.public(), SwarmOperation::AdvertiseService(ServiceRecord { path: path.clone(), provider: node.public(), allowed_users: [UserId::from_signing_key(&user)].into_iter().collect() }), &node);
    let removed = Commit::new([service.id].into_iter().collect(), node.public(), SwarmOperation::RemoveService { path: path.clone(), provider: node.public() }, &node);

    let store = atlas_swarm::MemoryStore::default();
    store.merge(vec![user_commit.clone(), service]).await.unwrap();
    let advertised = store.view().await.unwrap();
    assert_eq!(advertised.users[&UserId::from_signing_key(&user)].username.as_deref(), Some("ada"));
    assert!(advertised.services.contains_key(&path));
    let removed_store = atlas_swarm::MemoryStore::default();
    removed_store.merge(vec![user_commit, removed]).await.unwrap();
    assert!(!removed_store.view().await.unwrap().services.contains_key(&path));
}

#[tokio::test(flavor = "current_thread")]
async fn store_creates_local_commits_from_its_current_head() {
    let key = SecretKey::generate();
    let store = atlas_swarm::MemoryStore::default();
    let coordinate = NodeCoordinate::new(0.2, 0.8).unwrap();
    let joined = store.append_operation(key.public(), MembershipOperation::Join(NodeRecord { name: "laptop".into(), endpoint_id: key.public(), endpoint_addr: iroh::EndpointAddr::new(key.public()), coordinate }).into(), &key).await.unwrap();
    let renamed = store.append_operation(key.public(), MembershipOperation::Rename { name: "desktop".into() }.into(), &key).await.unwrap();
    assert_eq!(renamed.parents, [joined.id].into_iter().collect());
    assert!(store.view().await.unwrap().membership.nodes.contains_key("desktop"));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires UDP socket binding, which is unavailable in the sandbox"]
async fn allowlisted_user_can_open_an_rpc_service() {
    let swarm = atlas_swarm::Swarm::create("host", Arc::new(atlas_swarm::MemoryStore::default())).await.unwrap();
    let user = SigningKey::from_bytes(&[11; 32]);
    let path = ServicePath::new("test/echo").unwrap();
    swarm.serve::<EchoHandle, _>(path.clone(), [UserId::from_signing_key(&user)].into_iter().collect(), EchoService).await.unwrap();
    let peer = tokio::time::timeout(Duration::from_secs(5), swarm.service(&path, &user)).await.unwrap().unwrap();
    let handle = EchoHandle::new(peer);
    assert_eq!(tokio::time::timeout(Duration::from_secs(5), handle.echo("hello".into())).await.unwrap().unwrap(), "hello");
}
