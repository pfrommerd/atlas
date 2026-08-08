use std::collections::BTreeSet;

use atlas_swarm::{membership_view, Commit, MembershipOperation, NodeCoordinate, NodeRecord};
use iroh::SecretKey;

#[test]
fn lowest_commit_id_wins_a_concurrent_name_collision() {
    let first = SecretKey::generate();
    let second = SecretKey::generate();
    let coordinate = NodeCoordinate::new(0.2, 0.8).unwrap();
    let left = Commit::new(BTreeSet::new(), first.public(), MembershipOperation::Join(NodeRecord { name: "laptop".into(), endpoint_id: first.public(), coordinate }), &first);
    let right = Commit::new(BTreeSet::new(), second.public(), MembershipOperation::Join(NodeRecord { name: "laptop".into(), endpoint_id: second.public(), coordinate }), &second);
    assert!(left.verify() && right.verify());
    let view = membership_view([left.clone(), right.clone()]);
    assert_eq!(view.nodes["laptop"].endpoint_id, if left.id < right.id { first.public() } else { second.public() });
}
