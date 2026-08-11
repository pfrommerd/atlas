use std::collections::{BTreeMap, BTreeSet};

use iroh::EndpointId;

use crate::NodeCoordinate;

pub const SPANNER_STRETCH: f64 = 2.0;

pub fn neighbors(
    local: EndpointId,
    nodes: impl IntoIterator<Item = (EndpointId, NodeCoordinate, bool)>,
) -> BTreeSet<EndpointId> {
    let nodes: Vec<_> = nodes.into_iter().collect();
    let mut result = graph_neighbors(
        &local,
        nodes.iter().filter(|(_, _, down)| !down).map(copy_node),
    );
    result.extend(graph_neighbors(&local, nodes.iter().map(copy_node)));
    result.remove(&local);
    result
}

fn copy_node(
    (id, coordinate, _): &(EndpointId, NodeCoordinate, bool),
) -> (EndpointId, NodeCoordinate) {
    (*id, *coordinate)
}

fn graph_neighbors(
    local: &EndpointId,
    nodes: impl IntoIterator<Item = (EndpointId, NodeCoordinate)>,
) -> BTreeSet<EndpointId> {
    let nodes: Vec<_> = nodes.into_iter().collect();
    let mut candidates = Vec::new();
    for (index, (left, left_coordinate)) in nodes.iter().enumerate() {
        for (right, right_coordinate) in nodes.iter().skip(index + 1) {
            let distance = ((left_coordinate.x - right_coordinate.x).powi(2)
                + (left_coordinate.y - right_coordinate.y).powi(2))
            .sqrt();
            candidates.push((distance, *left, *right));
        }
    }
    candidates.sort_by(
        |(left_distance, left_a, left_b), (right_distance, right_a, right_b)| {
            left_distance
                .total_cmp(right_distance)
                .then_with(|| left_a.cmp(right_a))
                .then_with(|| left_b.cmp(right_b))
        },
    );

    let mut graph: BTreeMap<EndpointId, BTreeMap<EndpointId, f64>> = BTreeMap::new();
    for (distance, left, right) in candidates {
        let current = shortest_path(&graph, left, right);
        if current.is_none_or(|length| length > SPANNER_STRETCH * distance) {
            graph.entry(left).or_default().insert(right, distance);
            graph.entry(right).or_default().insert(left, distance);
        }
    }
    graph
        .remove(local)
        .unwrap_or_default()
        .into_keys()
        .collect()
}

fn shortest_path(
    graph: &BTreeMap<EndpointId, BTreeMap<EndpointId, f64>>,
    start: EndpointId,
    goal: EndpointId,
) -> Option<f64> {
    let mut distance: BTreeMap<EndpointId, f64> = BTreeMap::from([(start, 0.0)]);
    let mut visited = BTreeSet::new();
    loop {
        let (&node, &cost) = distance
            .iter()
            .filter(|(node, _)| !visited.contains(*node))
            .min_by(|(left_node, left_cost), (right_node, right_cost)| {
                (**left_cost)
                    .total_cmp(right_cost)
                    .then_with(|| left_node.cmp(right_node))
            })?;
        if node == goal {
            return Some(cost);
        }
        visited.insert(node);
        for (&next, &edge) in graph.get(&node).into_iter().flatten() {
            let next_cost = cost + edge;
            if distance.get(&next).is_none_or(|known| next_cost < *known) {
                distance.insert(next, next_cost);
            }
        }
    }
}
