use std::collections::BTreeSet;

use atlas_swarm::{Commit, PathAcl, RedbStore, Store, SwarmOperation, UserId};
use ed25519_dalek::SigningKey;
use iroh::SecretKey;

#[tokio::test]
async fn metadata_log_survives_reopening() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("swarm.redb");
    let endpoint = SecretKey::generate();
    let user = SigningKey::from_bytes(&[9; 32]);
    let user_id = UserId::from_signing_key(&user);
    let mut genesis = Commit::new_unsigned(
        BTreeSet::new(),
        endpoint.public(),
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
    genesis.sign_user(&user);
    let id = genesis.id;
    let store = RedbStore::open(&path).unwrap();
    store.append_commit(genesis, &endpoint).await.unwrap();
    drop(store);

    let reopened = RedbStore::open(path).unwrap();
    assert_eq!(reopened.commits().await.unwrap()[0].id, id);
    assert!(reopened.view().await.unwrap().swarm_id.is_some());
}
