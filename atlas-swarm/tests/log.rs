use std::collections::BTreeSet;

use atlas_swarm::{
    Commit, MemoryStore, PathAcl, PathOperation, PathResource, ServiceRecord, Store,
    SwarmOperation, SwarmPath, UserId,
};
use ed25519_dalek::SigningKey;
use iroh::SecretKey;

fn genesis(key: &SecretKey, user: &SigningKey) -> Commit {
    let user_id = UserId::from_signing_key(user);
    let mut commit = Commit::new_unsigned(
        BTreeSet::new(),
        key.public(),
        user_id,
        1,
        SwarmOperation::Genesis {
            swarm_id: uuid::Uuid::new_v4(),
            root_acl: PathAcl {
                readers: [user_id].into_iter().collect(),
                writers: [user_id].into_iter().collect(),
            },
        },
    );
    commit.sign_user(user);
    commit.sign_endpoint(key);
    commit
}

#[test]
fn dual_signatures_bind_parents_and_operation() {
    let endpoint = SecretKey::generate();
    let user = SigningKey::from_bytes(&[1; 32]);
    let mut commit = genesis(&endpoint, &user);
    assert!(commit.verify());
    commit.parents.insert(uuid::Uuid::new_v4());
    assert!(!commit.verify());
}

#[tokio::test]
async fn genesis_derives_swarm_identity_and_acl() {
    let endpoint = SecretKey::generate();
    let user = SigningKey::from_bytes(&[2; 32]);
    let commit = genesis(&endpoint, &user);
    let swarm_id = match commit.operation {
        SwarmOperation::Genesis { swarm_id, .. } => swarm_id,
        _ => unreachable!(),
    };
    let store = MemoryStore::default();
    store.merge(vec![commit]).await.unwrap();
    let view = store.view().await.unwrap();
    assert_eq!(view.swarm_id, Some(swarm_id));
    assert!(
        view.root_acl
            .unwrap()
            .writers
            .contains(&UserId::from_signing_key(&user))
    );
}

#[tokio::test]
async fn path_batches_are_applied_atomically() {
    let endpoint = SecretKey::generate();
    let service_endpoint = SecretKey::generate();
    let user = SigningKey::from_bytes(&[3; 32]);
    let user_id = UserId::from_signing_key(&user);
    let genesis = genesis(&endpoint, &user);
    let mut batch = Commit::new_unsigned(
        [genesis.id].into_iter().collect(),
        endpoint.public(),
        user_id,
        2,
        SwarmOperation::PathBatch(vec![
            PathOperation::DefineService {
                path: SwarmPath::new("/acp/test").unwrap(),
                service: ServiceRecord {
                    provider: service_endpoint.public(),
                    endpoint_addr: None,
                    allowed_users: [user_id].into_iter().collect(),
                },
            },
            PathOperation::SetConfig {
                path: SwarmPath::new("/nodes/test/acp").unwrap(),
                value: serde_json::json!({"command": "atlas-acp"}),
            },
        ]),
    );
    batch.sign_user(&user);
    batch.sign_endpoint(&endpoint);
    let store = MemoryStore::default();
    store.merge(vec![genesis, batch]).await.unwrap();
    let view = store.view().await.unwrap();
    assert!(matches!(
        view.paths[&SwarmPath::new("/acp/test").unwrap()]
            .resource
            .as_ref(),
        Some(PathResource::Service(_))
    ));
    assert!(matches!(
        view.paths[&SwarmPath::new("/nodes/test/acp").unwrap()]
            .resource
            .as_ref(),
        Some(PathResource::Config(_))
    ));
}

#[tokio::test]
async fn an_invalid_path_batch_applies_no_operations() {
    let endpoint = SecretKey::generate();
    let user = SigningKey::from_bytes(&[4; 32]);
    let genesis = genesis(&endpoint, &user);
    let path = SwarmPath::new("/collision").unwrap();
    let mut batch = Commit::new_unsigned(
        [genesis.id].into_iter().collect(),
        endpoint.public(),
        UserId::from_signing_key(&user),
        2,
        SwarmOperation::PathBatch(vec![
            PathOperation::SetConfig {
                path: path.clone(),
                value: serde_json::json!({"first": true}),
            },
            PathOperation::SetConfig {
                path: path.clone(),
                value: serde_json::json!({"second": true}),
            },
        ]),
    );
    batch.sign_user(&user);
    batch.sign_endpoint(&endpoint);
    let store = MemoryStore::default();
    store.merge(vec![genesis, batch]).await.unwrap();
    assert!(!store.view().await.unwrap().paths.contains_key(&path));
}
