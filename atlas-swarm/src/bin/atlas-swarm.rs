use std::{env, path::PathBuf, process, sync::Arc};

use atlas_swarm::{
    local::{
        autostart, autostart_at, connect_control, default_socket, reset_daemon, serve_daemon,
        LocalDaemon, StateSelector, StateSnapshot,
    },
    Commit, MemoryStore, PathAcl, PathOperation, PathResource, Swarm, SwarmOperation, SwarmPath,
    UserId,
};

fn usage() -> ! {
    eprintln!("usage:\n  atlas-swarm [--reset|-r] serve [--root-user USER_ID] [--join-path PATH] [--socket PATH] [--name NAME] [--bootstrap ENDPOINT_ID] [--reset|-r]\n  atlas-swarm [--reset|-r] log [--limit N] [--socket PATH] [--reset|-r]\n  atlas-swarm [--reset|-r] ls [PATH] [--socket PATH] [--reset|-r]\n  atlas-swarm [--reset|-r] node PATH [--socket PATH] [--reset|-r]\n  atlas-swarm [--reset|-r] rm PATH [--socket PATH] [--reset|-r]\n  atlas-swarm [--reset|-r] info [--socket PATH] [--reset|-r]\n  atlas-swarm [--reset|-r] users [--socket PATH] [--reset|-r]");
    process::exit(2);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments: Vec<_> = env::args().skip(1).collect();
    let mut reset = false;
    while arguments
        .first()
        .is_some_and(|argument| matches!(argument.as_str(), "--reset" | "-r"))
    {
        reset = true;
        arguments.remove(0);
    }
    let mut arguments = arguments.into_iter();
    let command = arguments.next().unwrap_or_else(|| usage());
    if command != "serve" {
        return client(command, arguments.collect(), reset).await;
    }
    let mut socket = None;
    let mut name = "atlas-swarm".to_owned();
    let mut bootstrap = None;
    let mut join_path = None;
    let mut root_user: Option<UserId> = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--reset" | "-r" => reset = true,
            "--socket" => socket = Some(arguments.next().unwrap_or_else(|| usage()).into()),
            "--name" => name = arguments.next().unwrap_or_else(|| usage()),
            "--bootstrap" => {
                bootstrap = Some(iroh::EndpointAddr::new(
                    arguments.next().unwrap_or_else(|| usage()).parse()?,
                ))
            }
            "--join-path" => {
                join_path = Some(
                    SwarmPath::new(arguments.next().unwrap_or_else(|| usage()))
                        .unwrap_or_else(|| usage()),
                )
            }
            "--root-user" => {
                root_user = Some(
                    arguments
                        .next()
                        .unwrap_or_else(|| usage())
                        .parse()
                        .unwrap_or_else(|_| usage()),
                )
            }
            _ => usage(),
        }
    }
    let signer = atlas_swarm::auth::UserSigner::discover().await?;
    let root_user = root_user.unwrap_or_else(|| signer.user());
    if bootstrap.is_none() && root_user != signer.user() {
        return Err("--root-user must be the local SSH identity".into());
    }
    let socket = socket.unwrap_or(default_socket()?);
    if reset {
        reset_daemon(&socket).await?;
    }
    let root_acl = PathAcl {
        readers: [root_user].into_iter().collect(),
        writers: [root_user].into_iter().collect(),
    };
    let swarm = Arc::new(
        Swarm::start(
            name,
            root_acl.clone(),
            bootstrap.clone(),
            Arc::new(MemoryStore::default()),
        )
        .await?,
    );
    let join_path = join_path.unwrap_or_else(|| {
        let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".into());
        SwarmPath::new(format!("/nodes/{host}")).expect("hostname produces a valid path")
    });
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    if bootstrap.is_none() {
        let mut genesis = Commit::new_unsigned(
            Default::default(),
            swarm.endpoint_id(),
            signer.user(),
            now,
            SwarmOperation::Genesis {
                swarm_id: uuid::Uuid::new_v4(),
                root_acl: root_acl.clone(),
            },
        );
        genesis.user_signature = signer.sign(&genesis.signing_bytes()).await?;
        swarm.submit_commit(genesis).await?;
    }
    let mut node_join = Commit::new_unsigned(
        swarm
            .store()
            .commits()
            .await
            .map_err(std::io::Error::other)?
            .into_iter()
            .map(|commit| commit.id)
            .collect(),
        swarm.endpoint_id(),
        signer.user(),
        now,
        SwarmOperation::Path(PathOperation::NodeJoin {
            path: join_path,
            node: atlas_swarm::NodeRecord {
                name: swarm.node_name().to_owned(),
                endpoint_id: swarm.endpoint_id(),
                endpoint_addr: swarm.endpoint_addr(),
                coordinate: swarm.node_coordinate(),
            },
        }),
    );
    node_join.user_signature = signer.sign(&node_join.signing_bytes()).await?;
    swarm.submit_commit(node_join).await?;
    serve_daemon(&socket, LocalDaemon::new(swarm)).await?;
    Ok(())
}

async fn client(
    command: String,
    arguments: Vec<String>,
    mut reset: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut socket: Option<PathBuf> = None;
    let mut positional = Vec::new();
    let mut limit = 16u32;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--reset" | "-r" => reset = true,
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
    let control = if reset {
        autostart_at(&socket.unwrap_or(default_socket()?), true).await?
    } else {
        match socket {
            Some(socket) => connect_control(&socket).await?,
            None => autostart().await?,
        }
    };
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
                        format!("repository ({} endpoints)", repository.endpoints.len())
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
        SwarmOperation::Path(PathOperation::SetConfig { path, .. }) => {
            format!("set config {}", path.as_str())
        }
        SwarmOperation::Path(PathOperation::Remove { path }) => format!("remove {}", path.as_str()),
    }
}
