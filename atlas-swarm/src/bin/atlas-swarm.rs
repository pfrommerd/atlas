use std::{env, path::PathBuf, process};

use atlas_swarm::{
    Commit, PathOperation, PathResource, SwarmOperation, SwarmPath,
    local::{StateSelector, StateSnapshot, connect_control, default_socket},
};

fn usage() -> ! {
    eprintln!(
        "usage:\n  atlas-swarm log [--limit N] [--socket PATH]\n  atlas-swarm ls [PATH] [--socket PATH]\n  atlas-swarm node PATH [--socket PATH]\n  atlas-swarm repo init SWARM_PATH DIRECTORY [--socket PATH]\n  atlas-swarm repo checkout SWARM_PATH DIRECTORY [--socket PATH]\n  atlas-swarm rm PATH [--socket PATH]\n  atlas-swarm info [--socket PATH]\n  atlas-swarm users [--socket PATH]\n  atlas-swarm jj JJ_ARGS..."
    );
    process::exit(2);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let arguments: Vec<_> = env::args().skip(1).collect();
    let mut arguments = arguments.into_iter();
    let command = arguments.next().unwrap_or_else(|| usage());
    if command == "jj" {
        let version = process::Command::new("jj").arg("--version").output()?;
        if !version.status.success()
            || String::from_utf8_lossy(&version.stdout).trim() != "jj 0.44.0"
        {
            return Err("atlas-swarm requires jj 0.44.0".into());
        }
        let status = process::Command::new("jj").args(arguments).status()?;
        process::exit(status.code().unwrap_or(1));
    }
    client(command, arguments.collect()).await
}

async fn client(command: String, arguments: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut socket: Option<PathBuf> = None;
    let mut positional = Vec::new();
    let mut limit = 16u32;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--socket" => socket = Some(arguments.next().unwrap_or_else(|| usage()).into()),
            "--limit" => {
                limit = arguments
                    .next()
                    .unwrap_or_else(|| usage())
                    .parse()
                    .unwrap_or_else(|_| usage())
            }
            value if value.starts_with('-') => usage(),
            value => positional.push(value.to_owned()),
        }
    }
    let socket = socket.unwrap_or(default_socket()?);
    let control = connect_control(&socket).await?;
    match command.as_str() {
        "log" => {
            if !positional.is_empty() {
                usage();
            }
            let history = control
                .commit_history(atlas_swarm::local::CommitHistoryRequest {
                    starts: Vec::new(),
                    depth: limit.saturating_sub(1),
                })
                .await
                .map_err(std::io::Error::other)?;
            let mut commits = history.commits;
            commits.sort_by(|a, b| {
                b.created_at_ms
                    .cmp(&a.created_at_ms)
                    .then_with(|| b.id.cmp(&a.id))
            });
            for commit in commits {
                let timestamp =
                    chrono::DateTime::from_timestamp_millis(commit.created_at_ms as i64)
                        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                        .unwrap_or_else(|| "invalid timestamp".into());
                println!(
                    "○ {} {} {} {} {}",
                    &commit.id.to_string()[..8],
                    timestamp,
                    &commit.author.to_string()[..8],
                    &commit.user.to_string()[..8],
                    summary(&commit.operation)
                );
            }
            if history.truncated {
                println!("│\n~");
            }
        }
        "ls" => {
            if positional.len() > 1 {
                usage();
            }
            let prefix = positional
                .pop()
                .map(|path| SwarmPath::new(path).unwrap_or_else(|| usage()));
            let StateSnapshot::Paths(paths) = control
                .query(StateSelector::Paths { prefix })
                .await
                .map_err(std::io::Error::other)?
            else {
                unreachable!()
            };
            for (path, entry) in paths {
                let kind = match entry.resource {
                    Some(PathResource::Service(service)) => format!("service {}", service.provider),
                    Some(PathResource::Node(node)) => format!("node {}", node.endpoint_id),
                    Some(PathResource::Repository(repository)) => {
                        format!(
                            "jj repository ({} endpoints, {} heads)",
                            repository.endpoints.len(),
                            repository.snapshot_heads.len()
                        )
                    }
                    Some(PathResource::Config(_)) => "config".into(),
                    None => "path".into(),
                };
                println!("{} [{kind}]", path.as_str());
            }
        }
        "node" => {
            if positional.len() != 1 {
                usage();
            }
            let path = SwarmPath::new(positional.pop().unwrap()).unwrap_or_else(|| usage());
            let StateSnapshot::Path(state) = control
                .query(StateSelector::Path { path: path.clone() })
                .await
                .map_err(std::io::Error::other)?
            else {
                unreachable!()
            };
            let Some(PathResource::Node(node)) = state.entry.and_then(|entry| entry.resource)
            else {
                return Err(format!("no node at {}", path.as_str()).into());
            };
            println!(
                "path: {}\nname: {}\nendpoint: {}\naddress: {:?}\ncoordinate: {}, {}",
                path.as_str(),
                node.name,
                node.endpoint_id,
                node.endpoint_addr,
                node.coordinate.x,
                node.coordinate.y
            );
        }
        "repo" => {
            if positional.len() != 3 {
                usage();
            }
            let action = positional.remove(0);
            let path = SwarmPath::new(positional.remove(0)).unwrap_or_else(|| usage());
            let directory = PathBuf::from(positional.remove(0));
            let signer = atlas_swarm::auth::UserSigner::discover().await?;
            let author = control
                .endpoint_id(())
                .await
                .map_err(std::io::Error::other)?;
            match action.as_str() {
                "init" => {
                    let StateSnapshot::Path(state) = control
                        .query(StateSelector::Path { path: path.clone() })
                        .await
                        .map_err(std::io::Error::other)?
                    else {
                        unreachable!()
                    };
                    if state.entry.is_some_and(|entry| entry.resource.is_some()) {
                        return Err(
                            format!("a resource already exists at {}", path.as_str()).into()
                        );
                    }
                    atlas_swarm::native_jj::init_workspace(&directory)
                        .await
                        .map_err(|error| std::io::Error::other(error.to_string()))?;
                    let history = control
                        .commit_history(atlas_swarm::local::CommitHistoryRequest {
                            starts: Vec::new(),
                            depth: 0,
                        })
                        .await
                        .map_err(std::io::Error::other)?;
                    let mut commit = Commit::new_unsigned(
                        history
                            .commits
                            .into_iter()
                            .map(|commit| commit.id)
                            .collect(),
                        author,
                        signer.user(),
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)?
                            .as_millis() as u64,
                        SwarmOperation::Path(PathOperation::DefineRepository {
                            path: path.clone(),
                            repository: atlas_swarm::RepositoryRecord {
                                id: uuid::Uuid::new_v4(),
                                kind: atlas_swarm::RepositoryKind::Jujutsu {
                                    format_version: atlas_swarm::JJ_REPOSITORY_FORMAT_VERSION,
                                },
                                endpoints: [author].into_iter().collect(),
                                allowed_users: [signer.user()].into_iter().collect(),
                                snapshot_heads: Default::default(),
                            },
                        }),
                    );
                    commit.user_signature = signer.sign(&commit.signing_bytes()).await?;
                    control
                        .submit_commit(commit)
                        .await
                        .map_err(std::io::Error::other)?;
                    println!("initialized {} at {}", path.as_str(), directory.display());
                }
                "checkout" => {
                    let StateSnapshot::Path(state) = control
                        .query(StateSelector::Path { path: path.clone() })
                        .await
                        .map_err(std::io::Error::other)?
                    else {
                        unreachable!()
                    };
                    let Some(PathResource::Repository(repository)) =
                        state.entry.and_then(|entry| entry.resource)
                    else {
                        return Err(format!("no repository at {}", path.as_str()).into());
                    };
                    if !repository.allowed_users.contains(&signer.user()) {
                        return Err(format!("repository access denied: {}", path.as_str()).into());
                    }
                    if !repository.snapshot_heads.is_empty() {
                        return Err("checkout of a published repository requires repository object synchronization".into());
                    }
                    atlas_swarm::native_jj::init_workspace(&directory)
                        .await
                        .map_err(|error| std::io::Error::other(error.to_string()))?;
                    println!(
                        "checked out empty {} at {}",
                        path.as_str(),
                        directory.display()
                    );
                }
                _ => usage(),
            }
        }
        "rm" => {
            if positional.len() != 1 {
                usage();
            }
            let path = SwarmPath::new(positional.pop().unwrap()).unwrap_or_else(|| usage());
            let StateSnapshot::Path(state) = control
                .query(StateSelector::Path { path: path.clone() })
                .await
                .map_err(std::io::Error::other)?
            else {
                unreachable!()
            };
            if !state.entry.is_some_and(|entry| entry.resource.is_some()) {
                return Err(format!("no resource at {}", path.as_str()).into());
            }
            let signer = atlas_swarm::auth::UserSigner::discover().await?;
            let author = control
                .endpoint_id(())
                .await
                .map_err(std::io::Error::other)?;
            let history = control
                .commit_history(atlas_swarm::local::CommitHistoryRequest {
                    starts: Vec::new(),
                    depth: 0,
                })
                .await
                .map_err(std::io::Error::other)?;
            let mut commit = Commit::new_unsigned(
                history
                    .commits
                    .into_iter()
                    .map(|commit| commit.id)
                    .collect(),
                author,
                signer.user(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_millis() as u64,
                SwarmOperation::Path(PathOperation::Remove { path }),
            );
            commit.user_signature = signer.sign(&commit.signing_bytes()).await?;
            control
                .submit_commit(commit)
                .await
                .map_err(std::io::Error::other)?;
        }
        "info" => {
            if !positional.is_empty() {
                usage();
            }
            let info = control.info(()).await.map_err(std::io::Error::other)?;
            println!(
                "swarm: {}",
                info.swarm_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| "-".into())
            );
            {
                let acl = info.root_acl;
                println!("root ACL");
                println!("readers:");
                for user in acl.readers {
                    println!("  {user}");
                }
                println!("writers:");
                for user in acl.writers {
                    println!("  {user}");
                }
            }
        }
        "users" => {
            if !positional.is_empty() {
                usage();
            }
            let StateSnapshot::Users(users) = control
                .query(StateSelector::Users)
                .await
                .map_err(std::io::Error::other)?
            else {
                unreachable!()
            };
            for (user, metadata) in users {
                let username = metadata.username.as_deref().unwrap_or("-");
                let real_name = metadata.real_name.as_deref().unwrap_or("-");
                println!("{user}\t{username}\t{real_name}");
            }
        }
        _ => usage(),
    }
    Ok(())
}

fn summary(operation: &SwarmOperation) -> String {
    match operation {
        SwarmOperation::Genesis { swarm_id, .. } => format!("genesis {swarm_id}"),
        SwarmOperation::Membership(atlas_swarm::MembershipOperation::Join(node)) => {
            format!("join {}", node.name)
        }
        SwarmOperation::Membership(atlas_swarm::MembershipOperation::Rename { name }) => {
            format!("rename {name}")
        }
        SwarmOperation::Membership(atlas_swarm::MembershipOperation::MarkDown { node }) => {
            format!("mark down {node}")
        }
        SwarmOperation::Membership(atlas_swarm::MembershipOperation::MarkUp) => "mark up".into(),
        SwarmOperation::UserMetadata(_) => "update user metadata".into(),
        SwarmOperation::Path(PathOperation::SetAcl { path, .. }) => format!("set ACL {path:?}"),
        SwarmOperation::Path(PathOperation::NodeJoin { path, .. }) => {
            format!("join node {}", path.as_str())
        }
        SwarmOperation::Path(PathOperation::NodeMove { from, to, .. }) => {
            format!("move node {} → {}", from.as_str(), to.as_str())
        }
        SwarmOperation::Path(PathOperation::DefineService { path, .. }) => {
            format!("define service {}", path.as_str())
        }
        SwarmOperation::Path(PathOperation::DefineRepository { path, .. }) => {
            format!("define repository {}", path.as_str())
        }
        SwarmOperation::Path(PathOperation::PublishRepositorySnapshot {
            path, snapshot, ..
        }) => {
            format!("publish repository {} at {}", path.as_str(), snapshot)
        }
        SwarmOperation::Path(PathOperation::SetConfig { path, .. }) => {
            format!("set config {}", path.as_str())
        }
        SwarmOperation::Path(PathOperation::Remove { path }) => format!("remove {}", path.as_str()),
        SwarmOperation::PathBatch(operations) => {
            format!("apply {} path operations", operations.len())
        }
    }
}
