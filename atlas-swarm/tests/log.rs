use std::collections::BTreeSet;

use atlas_swarm::{Commit, PathAcl, SwarmOperation, UserId};
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
    use atlas_swarm::{MemoryStore, Store};
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
    assert!(view
        .root_acl
        .unwrap()
        .writers
        .contains(&UserId::from_signing_key(&user)));
}
