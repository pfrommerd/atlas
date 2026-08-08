use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use iroh::{EndpointId, SecretKey, Signature};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type CommitId = Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeCoordinate {
    pub x: f64,
    pub y: f64,
}

impl NodeCoordinate {
    pub fn new(x: f64, y: f64) -> Option<Self> {
        (x.is_finite() && y.is_finite()).then_some(Self { x, y })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeRecord {
    pub name: String,
    pub endpoint_id: EndpointId,
    pub coordinate: NodeCoordinate,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum MembershipOperation {
    Join(NodeRecord),
    MarkDown { node: EndpointId },
    MarkUp,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Commit {
    pub id: CommitId,
    pub parents: BTreeSet<CommitId>,
    pub author: EndpointId,
    pub operation: MembershipOperation,
    pub signature: Vec<u8>,
}

impl Commit {
    pub fn new(parents: BTreeSet<CommitId>, author: EndpointId, operation: MembershipOperation, key: &SecretKey) -> Self {
        let mut commit = Self { id: Uuid::new_v4(), parents, author, operation, signature: Vec::new() };
        commit.signature = key.sign(&commit.signing_bytes()).to_bytes().to_vec();
        commit
    }

    pub fn verify(&self) -> bool {
        let Ok(bytes) = self.signature.as_slice().try_into() else { return false };
        self.author.verify(&self.signing_bytes(), &Signature::from_bytes(bytes)).is_ok()
    }

    fn signing_bytes(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        serde_cbor::to_vec(&unsigned).expect("commit serialization cannot fail")
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MembershipView {
    pub nodes: BTreeMap<String, NodeRecord>,
    pub down: BTreeSet<EndpointId>,
}

impl MembershipView {
    pub fn is_down(&self, id: &EndpointId) -> bool {
        self.down.contains(id)
    }
}

pub fn membership_view(commits: impl IntoIterator<Item = Commit>) -> MembershipView {
    let commits: HashMap<_, _> = commits.into_iter().map(|commit| (commit.id, commit)).collect();
    let mut joins_by_name: BTreeMap<String, (CommitId, NodeRecord)> = BTreeMap::new();
    let mut status: HashMap<EndpointId, (CommitId, bool)> = HashMap::new();

    for commit in commits.values() {
        match &commit.operation {
            MembershipOperation::Join(node) => {
                joins_by_name
                    .entry(node.name.clone())
                    .and_modify(|current| {
                        if commit.id < current.0 {
                            *current = (commit.id, node.clone());
                        }
                    })
                    .or_insert_with(|| (commit.id, node.clone()));
            }
            MembershipOperation::MarkDown { node } => {
                update_status(&commits, &mut status, *node, commit.id, true);
            }
            MembershipOperation::MarkUp => {
                update_status(&commits, &mut status, commit.author, commit.id, false);
            }
        }
    }

    MembershipView {
        nodes: joins_by_name.into_values().map(|(_, node)| (node.name.clone(), node)).collect(),
        down: status
            .into_iter()
            .filter_map(|(node, (_, is_down))| is_down.then_some(node))
            .collect(),
    }
}

fn update_status(
    commits: &HashMap<CommitId, Commit>,
    status: &mut HashMap<EndpointId, (CommitId, bool)>,
    node: EndpointId,
    id: CommitId,
    is_down: bool,
) {
    let Some((current_id, _)) = status.get(&node).copied() else {
        status.insert(node, (id, is_down));
        return;
    };

    let replace = if is_ancestor(commits, current_id, id) {
        true
    } else if is_ancestor(commits, id, current_id) {
        false
    } else {
        id < current_id
    };
    if replace {
        status.insert(node, (id, is_down));
    }
}

fn is_ancestor(commits: &HashMap<CommitId, Commit>, ancestor: CommitId, descendant: CommitId) -> bool {
    let mut pending = vec![descendant];
    let mut seen = HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(commit) = commits.get(&id) else {
            continue;
        };
        if commit.parents.contains(&ancestor) {
            return true;
        }
        pending.extend(commit.parents.iter().copied());
    }
    false
}
