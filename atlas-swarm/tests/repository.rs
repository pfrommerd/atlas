use std::{collections::BTreeSet, process::Command};

use atlas_swarm::{
    JJ_REPOSITORY_FORMAT_VERSION, RepositorySnapshotId, native_jj,
    repository::{JujutsuSnapshot, ObjectKind, RepositoryDatabase},
};

#[tokio::test]
async fn native_workspace_is_usable_by_jj_0_44() {
    let directory = tempfile::tempdir().unwrap();
    native_jj::init_workspace(directory.path()).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(directory.path().join(".jj/repo/store/type")).unwrap(),
        "Simple"
    );
    let output = Command::new("jj")
        .args(["status", "--no-pager"])
        .current_dir(directory.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "jj status failed: {}",
        String::from_utf8_lossy(&output.stderr)
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
        operation_heads: BTreeSet::from([vec![1, 2, 3]]),
    };
    let first = database.write_snapshot(repository_id, &snapshot).unwrap();
    let second = database.write_snapshot(repository_id, &snapshot).unwrap();
    assert_eq!(first, second);
    assert_ne!(first, RepositorySnapshotId([0; 32]));
    assert_eq!(
        database.read_snapshot(repository_id, &first).unwrap(),
        Some(snapshot)
    );
}
