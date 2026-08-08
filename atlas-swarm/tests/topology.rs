use atlas_swarm::{neighbors, NodeCoordinate};
use iroh::SecretKey;

fn node(x: f64, y: f64) -> (iroh::EndpointId, NodeCoordinate) {
    (SecretKey::generate().public(), NodeCoordinate::new(x, y).unwrap())
}

#[test]
fn greedy_spanner_omits_an_edge_when_an_equally_short_path_exists() {
    let left = node(0.0, 0.0);
    let middle = node(1.0, 0.0);
    let right = node(2.0, 0.0);
    let result = neighbors(
        left.0,
        [(left.0, left.1, false), (middle.0, middle.1, false), (right.0, right.1, false)],
    );
    assert_eq!(result.into_iter().collect::<Vec<_>>(), vec![middle.0]);
}

#[test]
fn down_peers_remain_reachable_through_the_all_peers_spanner() {
    let left = node(0.0, 0.0);
    let middle = node(1.0, 0.0);
    let right = node(2.0, 0.0);
    let result = neighbors(
        left.0,
        [(left.0, left.1, false), (middle.0, middle.1, true), (right.0, right.1, false)],
    );
    assert!(result.contains(&middle.0));
}
