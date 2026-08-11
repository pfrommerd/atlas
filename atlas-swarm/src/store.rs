use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use iroh::{EndpointId, SecretKey};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    Commit, CommitId, MembershipOperation, MembershipView, NodeCoordinate, PathAcl, PathEntry,
    PathOperation, PathResource, SwarmOperation, SwarmPath, SwarmView, UserId, UserMetadata,
};

#[derive(Clone, Debug, PartialEq)]
pub struct StoredIdentity {
    pub swarm_id: Uuid,
    pub secret_key: [u8; 32],
    pub node_name: String,
    pub coordinate: NodeCoordinate,
}

#[async_trait]
pub trait Store: Send + Sync + 'static {
    async fn load_identity(
        &self,
    ) -> Result<Option<StoredIdentity>, Box<dyn std::error::Error + Send + Sync>>;
    async fn save_identity(
        &self,
        identity: StoredIdentity,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
    async fn commits(&self) -> Result<Vec<Commit>, Box<dyn std::error::Error + Send + Sync>>;
    async fn append_operation(
        &self,
        author: EndpointId,
        operation: SwarmOperation,
        key: &SecretKey,
    ) -> Result<Commit, Box<dyn std::error::Error + Send + Sync>>;
    async fn merge(
        &self,
        commits: Vec<Commit>,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>>;
    async fn view(&self) -> Result<SwarmView, Box<dyn std::error::Error + Send + Sync>>;
}

#[derive(Clone, Default)]
pub struct MemoryStore(Arc<Mutex<MemoryState>>);

#[derive(Default)]
struct MemoryState {
    identity: Option<StoredIdentity>,
    commits: BTreeMap<CommitId, Commit>,
    view: SwarmView,
}

#[async_trait]
impl Store for MemoryStore {
    async fn load_identity(
        &self,
    ) -> Result<Option<StoredIdentity>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.0.lock().await.identity.clone())
    }

    async fn save_identity(
        &self,
        identity: StoredIdentity,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.0.lock().await.identity = Some(identity);
        Ok(())
    }

    async fn commits(&self) -> Result<Vec<Commit>, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.0.lock().await.commits.values().cloned().collect())
    }

    async fn append_operation(
        &self,
        author: EndpointId,
        operation: SwarmOperation,
        key: &SecretKey,
    ) -> Result<Commit, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.0.lock().await;
        let parents = state
            .commits
            .values()
            .filter(|commit| {
                !state
                    .commits
                    .values()
                    .any(|other| other.parents.contains(&commit.id))
            })
            .map(|commit| commit.id)
            .collect();
        let commit = Commit::new(parents, author, operation, key);
        state.commits.insert(commit.id, commit.clone());
        state.view = resolve_view(state.commits.values().cloned());
        Ok(commit)
    }

    async fn merge(
        &self,
        commits: Vec<Commit>,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.0.lock().await;
        let mut changed = false;
        for commit in commits.into_iter().filter(|commit| commit.verify()) {
            if state.commits.insert(commit.id, commit).is_none() {
                changed = true;
            }
        }
        if changed {
            state.view = resolve_view(state.commits.values().cloned());
        }
        Ok(changed)
    }

    async fn view(&self) -> Result<SwarmView, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.0.lock().await.view.clone())
    }
}

pub fn resolve_view(commits: impl IntoIterator<Item = Commit>) -> SwarmView {
    let commits: HashMap<_, _> = commits
        .into_iter()
        .map(|commit| (commit.id, commit))
        .collect();
    let mut nodes: HashMap<EndpointId, (CommitId, crate::NodeRecord)> = HashMap::new();
    let mut status: HashMap<EndpointId, (CommitId, bool)> = HashMap::new();
    let mut users: HashMap<UserId, (CommitId, UserMetadata)> = HashMap::new();
    let mut root_acl: Option<(CommitId, PathAcl)> = None;
    let mut paths: HashMap<SwarmPath, (CommitId, PathEntry)> = HashMap::new();
    for commit in commits.values() {
        match &commit.operation {
            SwarmOperation::Membership(MembershipOperation::Join(node)) => update_value(
                &commits,
                &mut nodes,
                node.endpoint_id,
                commit.id,
                node.clone(),
            ),
            SwarmOperation::Membership(MembershipOperation::Rename { .. }) => {}
            SwarmOperation::Membership(MembershipOperation::MarkDown { node }) => {
                update_value(&commits, &mut status, *node, commit.id, true)
            }
            SwarmOperation::Membership(MembershipOperation::MarkUp) => {
                update_value(&commits, &mut status, commit.author, commit.id, false)
            }
            SwarmOperation::UserMetadata(value) if value.verify() => update_value(
                &commits,
                &mut users,
                value.user,
                commit.id,
                value.metadata.clone(),
            ),
            SwarmOperation::InitializePathTree(value) if value.verify() => update_option(
                &commits,
                &mut root_acl,
                commit.id,
                match &value.operation {
                    PathOperation::SetAcl { path: None, acl } => Some(acl.clone()),
                    _ => None,
                },
            ),
            _ => {}
        }
    }
    let mut root_acl = root_acl.map(|(_, acl)| acl);
    let mut ordered: Vec<_> = commits.values().collect();
    ordered.sort_by(|left, right| {
        if is_ancestor(&commits, left.id, right.id) {
            std::cmp::Ordering::Less
        } else if is_ancestor(&commits, right.id, left.id) {
            std::cmp::Ordering::Greater
        } else {
            left.id.cmp(&right.id)
        }
    });
    for commit in ordered {
        let SwarmOperation::Path(value) = &commit.operation else {
            continue;
        };
        if !value.verify() || !can_write(&root_acl, &paths, path_of(&value.operation), value.user) {
            continue;
        }
        match &value.operation {
            PathOperation::SetAcl {
                path: Some(path),
                acl,
            } => {
                let entry = paths
                    .get(path)
                    .map(|(_, entry)| entry.clone())
                    .unwrap_or_default();
                let mut entry = entry;
                entry.acl = Some(acl.clone());
                update_value(&commits, &mut paths, path.clone(), commit.id, entry);
            }
            PathOperation::SetAcl { path: None, acl } => root_acl = Some(acl.clone()),
            PathOperation::DefineService { path, service } => set_resource(
                &commits,
                &mut paths,
                path.clone(),
                commit.id,
                PathResource::Service(service.clone()),
            ),
            PathOperation::DefineRepository { path, repository } => set_resource(
                &commits,
                &mut paths,
                path.clone(),
                commit.id,
                PathResource::Repository(repository.clone()),
            ),
            PathOperation::RemoveResource { path } => {
                if let Some((_, entry)) = paths.get(path).cloned() {
                    let mut entry = entry;
                    entry.resource = None;
                    update_value(&commits, &mut paths, path.clone(), commit.id, entry);
                }
            }
        }
    }
    for commit in commits.values() {
        let SwarmOperation::Membership(MembershipOperation::Rename { name }) = &commit.operation
        else {
            continue;
        };
        if let Some((_, node)) = nodes.get(&commit.author).cloned() {
            let mut node = node;
            node.name = name.clone();
            update_value(&commits, &mut nodes, commit.author, commit.id, node);
        }
    }
    let mut names: BTreeMap<String, (CommitId, crate::NodeRecord)> = BTreeMap::new();
    for (id, node) in nodes.into_values() {
        names
            .entry(node.name.clone())
            .and_modify(|current| {
                if id < current.0 {
                    *current = (id, node.clone());
                }
            })
            .or_insert((id, node));
    }
    let mut users: BTreeMap<_, _> = users
        .into_iter()
        .map(|(user, (_, value))| (user, value))
        .collect();
    let mut usernames: BTreeMap<String, (UserId, CommitId)> = BTreeMap::new();
    for commit in commits.values() {
        let SwarmOperation::UserMetadata(value) = &commit.operation else {
            continue;
        };
        if !value.verify() || users.get(&value.user) != Some(&value.metadata) {
            continue;
        }
        if let Some(username) = &value.metadata.username {
            usernames
                .entry(username.clone())
                .and_modify(|winner| {
                    if commit.id < winner.1 {
                        *winner = (value.user, commit.id);
                    }
                })
                .or_insert((value.user, commit.id));
        }
    }
    for (user, metadata) in &mut users {
        if metadata
            .username
            .as_ref()
            .is_some_and(|name| usernames.get(name).is_some_and(|winner| winner.0 != *user))
        {
            metadata.username = None;
        }
    }
    SwarmView {
        membership: MembershipView {
            nodes: names
                .into_values()
                .map(|(_, node)| (node.name.clone(), node))
                .collect(),
            down: status
                .into_iter()
                .filter_map(|(node, (_, down))| down.then_some(node))
                .collect(),
        },
        users,
        root_acl,
        paths: paths
            .into_iter()
            .map(|(path, (_, entry))| (path, entry))
            .collect(),
    }
}

fn path_of(operation: &PathOperation) -> Option<&SwarmPath> {
    match operation {
        PathOperation::SetAcl { path, .. } => path.as_ref(),
        PathOperation::DefineService { path, .. }
        | PathOperation::DefineRepository { path, .. }
        | PathOperation::RemoveResource { path } => Some(path),
    }
}

fn can_write(
    root: &Option<PathAcl>,
    paths: &HashMap<SwarmPath, (CommitId, PathEntry)>,
    path: Option<&SwarmPath>,
    user: UserId,
) -> bool {
    let Some(path) = path else {
        return root.as_ref().is_some_and(|acl| acl.writers.contains(&user));
    };
    let mut acl = root.as_ref();
    for ancestor in path
        .as_str()
        .split('/')
        .scan(String::new(), |prefix, segment| {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            SwarmPath::new(prefix.clone())
        })
    {
        if let Some((_, entry)) = paths.get(&ancestor) {
            if entry.acl.is_some() {
                acl = entry.acl.as_ref();
            }
        }
    }
    acl.is_some_and(|acl| acl.writers.contains(&user))
}

fn set_resource(
    commits: &HashMap<CommitId, Commit>,
    paths: &mut HashMap<SwarmPath, (CommitId, PathEntry)>,
    path: SwarmPath,
    id: CommitId,
    resource: PathResource,
) {
    let entry = paths
        .get(&path)
        .map(|(_, entry)| entry.clone())
        .unwrap_or_default();
    if entry
        .resource
        .as_ref()
        .is_some_and(|current| std::mem::discriminant(current) != std::mem::discriminant(&resource))
    {
        return;
    }
    let mut entry = entry;
    entry.resource = Some(resource);
    update_value(commits, paths, path, id, entry);
}

fn update_option<V>(
    commits: &HashMap<CommitId, Commit>,
    value: &mut Option<(CommitId, V)>,
    id: CommitId,
    candidate: Option<V>,
) {
    let Some(candidate) = candidate else {
        return;
    };
    if value.as_ref().is_none_or(|(current, _)| {
        is_ancestor(commits, *current, id) || (!is_ancestor(commits, id, *current) && id < *current)
    }) {
        *value = Some((id, candidate));
    }
}

fn update_value<K: Eq + std::hash::Hash, V>(
    commits: &HashMap<CommitId, Commit>,
    values: &mut HashMap<K, (CommitId, V)>,
    key: K,
    id: CommitId,
    value: V,
) {
    let replace = values.get(&key).is_none_or(|(current, _)| {
        is_ancestor(commits, *current, id) || (!is_ancestor(commits, id, *current) && id < *current)
    });
    if replace {
        values.insert(key, (id, value));
    }
}

fn is_ancestor(
    commits: &HashMap<CommitId, Commit>,
    ancestor: CommitId,
    descendant: CommitId,
) -> bool {
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
