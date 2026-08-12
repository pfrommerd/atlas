use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use async_trait::async_trait;
use iroh::{EndpointId, SecretKey};
use tokio::sync::Mutex;

use crate::{
    Commit, CommitId, MembershipOperation, MembershipView, NodeCoordinate, PathAcl, PathEntry,
    PathOperation, PathResource, SwarmOperation, SwarmPath, SwarmView, UserId, UserMetadata,
};

#[derive(Clone, Debug, PartialEq)]
pub struct StoredIdentity {
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
    async fn append_commit(
        &self,
        commit: Commit,
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

    async fn append_commit(
        &self,
        mut commit: Commit,
        key: &SecretKey,
    ) -> Result<Commit, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.0.lock().await;
        if commit.author != key.public()
            || !commit.endpoint_signature.is_empty()
            || !commit.verify_user()
        {
            return Err("invalid local commit signature".into());
        }
        let mut all = state.commits.clone();
        if all.insert(commit.id, commit.clone()).is_some() || !valid_history(&all) {
            return Err("invalid commit ancestry".into());
        }
        commit.sign_endpoint(key);
        state.commits.insert(commit.id, commit.clone());
        state.view = resolve_view(state.commits.values().cloned());
        Ok(commit)
    }

    async fn merge(
        &self,
        commits: Vec<Commit>,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        let mut state = self.0.lock().await;
        let mut next = state.commits.clone();
        for commit in commits {
            if !commit.verify() {
                return Err("invalid replicated commit signature".into());
            }
            next.entry(commit.id).or_insert(commit);
        }
        if !valid_history(&next) {
            return Err("invalid replicated commit ancestry".into());
        }
        let changed = next.len() != state.commits.len();
        state.commits = next;
        if changed {
            state.view = resolve_view(state.commits.values().cloned());
        }
        Ok(changed)
    }

    async fn view(&self) -> Result<SwarmView, Box<dyn std::error::Error + Send + Sync>> {
        let state = self.0.lock().await;
        Ok(resolve_view(state.commits.values().cloned()))
    }
}

pub fn resolve_view(commits: impl IntoIterator<Item = Commit>) -> SwarmView {
    let commits: HashMap<_, _> = commits
        .into_iter()
        .map(|commit| (commit.id, commit))
        .collect();
    let mut status: HashMap<EndpointId, (CommitId, bool)> = HashMap::new();
    let mut users: HashMap<UserId, (CommitId, UserMetadata)> = HashMap::new();
    let genesis = commits.values().find_map(|commit| match &commit.operation {
        SwarmOperation::Genesis { swarm_id, root_acl } => Some((*swarm_id, root_acl.clone())),
        _ => None,
    });
    let swarm_id = genesis.as_ref().map(|(id, _)| *id);
    let mut root_acl = genesis.map(|(_, acl)| acl);
    let mut paths: HashMap<SwarmPath, (CommitId, PathEntry)> = HashMap::new();
    for commit in commits.values() {
        match &commit.operation {
            SwarmOperation::Membership(MembershipOperation::Join(_))
            | SwarmOperation::Membership(MembershipOperation::Rename { .. }) => {}
            SwarmOperation::Membership(MembershipOperation::MarkDown { node }) => {
                update_value(&commits, &mut status, *node, commit.id, true)
            }
            SwarmOperation::Membership(MembershipOperation::MarkUp) => {
                update_value(&commits, &mut status, commit.author, commit.id, false)
            }
            SwarmOperation::UserMetadata(value) => {
                update_value(&commits, &mut users, commit.user, commit.id, value.clone())
            }
            _ => {}
        }
    }
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
        let allowed = match value {
            PathOperation::NodeMove { from, to, .. } => {
                can_write(&root_acl, &paths, Some(from), commit.user)
                    && can_write(&root_acl, &paths, Some(to), commit.user)
            }
            operation => can_write(&root_acl, &paths, path_of(operation), commit.user),
        };
        if !allowed {
            continue;
        }
        match value {
            PathOperation::SetAcl { path, acl } if path.as_str() != "/" => {
                let entry = paths
                    .get(path)
                    .map(|(_, entry)| entry.clone())
                    .unwrap_or_default();
                let mut entry = entry;
                entry.acl = Some(acl.clone());
                update_value(&commits, &mut paths, path.clone(), commit.id, entry);
            }
            PathOperation::SetAcl { path, acl } if !acl.writers.is_empty() => {
                root_acl = Some(acl.clone())
            }
            PathOperation::SetAcl { .. } => {}
            PathOperation::NodeJoin { path, node } if node.endpoint_id == commit.author => {
                set_resource(
                    &commits,
                    &mut paths,
                    path.clone(),
                    commit.id,
                    PathResource::Node(node.clone()),
                )
            }
            PathOperation::NodeJoin { .. } => {}
            PathOperation::NodeMove { node, from, to } => {
                let Some((_, source)) = paths.get(from).cloned() else {
                    continue;
                };
                if !matches!(source.resource, Some(PathResource::Node(ref record)) if record.endpoint_id == *node)
                {
                    continue;
                }
                let mut source = source;
                let resource = source.resource.take().expect("node resource checked above");
                update_value(&commits, &mut paths, from.clone(), commit.id, source);
                set_resource(&commits, &mut paths, to.clone(), commit.id, resource);
            }
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
            PathOperation::SetConfig { path, value } => set_resource(
                &commits,
                &mut paths,
                path.clone(),
                commit.id,
                PathResource::Config(value.clone()),
            ),
            PathOperation::Remove { path } => {
                if let Some((_, entry)) = paths.get(path).cloned() {
                    let mut entry = entry;
                    entry.resource = None;
                    update_value(&commits, &mut paths, path.clone(), commit.id, entry);
                }
            }
        }
    }
    let mut names: BTreeMap<String, (CommitId, crate::NodeRecord)> = BTreeMap::new();
    for (id, entry) in paths.values() {
        let Some(PathResource::Node(node)) = &entry.resource else {
            continue;
        };
        names
            .entry(node.name.clone())
            .and_modify(|current| {
                if *id < current.0 {
                    *current = (*id, node.clone());
                }
            })
            .or_insert((*id, node.clone()));
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
        if users.get(&commit.user) != Some(value) {
            continue;
        }
        if let Some(username) = &value.username {
            usernames
                .entry(username.clone())
                .and_modify(|winner| {
                    if commit.id < winner.1 {
                        *winner = (commit.user, commit.id);
                    }
                })
                .or_insert((commit.user, commit.id));
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
        swarm_id,
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

fn valid_history(commits: &BTreeMap<CommitId, Commit>) -> bool {
    let genesis: Vec<_> = commits
        .values()
        .filter(|commit| matches!(commit.operation, SwarmOperation::Genesis { .. }))
        .collect();
    if genesis.len() != 1 || !genesis[0].parents.is_empty() {
        return false;
    }
    if matches!(&genesis[0].operation, SwarmOperation::Genesis { root_acl, .. } if root_acl.writers.is_empty() || !root_acl.writers.contains(&genesis[0].user))
    {
        return false;
    }
    for commit in commits.values() {
        if !commits.contains_key(&commit.id)
            || (!matches!(commit.operation, SwarmOperation::Genesis { .. })
                && commit.parents.is_empty())
        {
            return false;
        }
        if !commit
            .parents
            .iter()
            .all(|parent| commits.contains_key(parent))
        {
            return false;
        }
        let mut pending = vec![commit.id];
        let mut seen = HashSet::new();
        let mut reaches_genesis = false;
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                return false;
            }
            if id == genesis[0].id {
                reaches_genesis = true;
                continue;
            }
            pending.extend(commits[&id].parents.iter().copied());
        }
        if !reaches_genesis {
            return false;
        }
    }
    true
}

fn path_of(operation: &PathOperation) -> Option<&SwarmPath> {
    match operation {
        PathOperation::SetAcl { path, .. } => Some(path),
        PathOperation::NodeJoin { path, .. } => Some(path),
        PathOperation::NodeMove { from, .. } => Some(from),
        PathOperation::DefineService { path, .. }
        | PathOperation::DefineRepository { path, .. }
        | PathOperation::SetConfig { path, .. }
        | PathOperation::Remove { path } => Some(path),
    }
}

fn can_write(
    root: &Option<PathAcl>,
    paths: &HashMap<SwarmPath, (CommitId, PathEntry)>,
    path: Option<&SwarmPath>,
    user: UserId,
) -> bool {
    let Some(path) = path else { return false };
    let mut writers = root
        .as_ref()
        .map(|acl| acl.writers.clone())
        .unwrap_or_default();
    if path.as_str() == "/" {
        return writers.contains(&user);
    }
    for ancestor in
        path.as_str()
            .trim_start_matches('/')
            .split('/')
            .scan(String::new(), |prefix, segment| {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(segment);
                SwarmPath::new(format!("/{prefix}"))
            })
    {
        if let Some((_, entry)) = paths.get(&ancestor) {
            if let Some(acl) = &entry.acl {
                writers.extend(acl.writers.iter().copied());
            }
        }
    }
    writers.contains(&user)
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
    let mut entry = entry;
    entry.resource = Some(resource);
    update_value(commits, paths, path, id, entry);
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
