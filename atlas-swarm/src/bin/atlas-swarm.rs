use std::{env, path::PathBuf, process, sync::Arc};

use atlas_swarm::{
    local::{
        autostart, connect_control, default_socket, serve_daemon, LocalDaemon, StateSelector,
        StateSnapshot,
    },
    MemoryStore, PathAcl, PathResource, Swarm, SwarmPath, UserId,
};

fn usage() -> ! {
    eprintln!("usage:\n  atlas-swarm serve --root-user USER_ID [--socket PATH] [--name NAME] [--bootstrap ENDPOINT_ID]\n  atlas-swarm ls [PATH] [--socket PATH]\n  atlas-swarm info [--socket PATH]\n  atlas-swarm users [--socket PATH]");
    process::exit(2);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    let command = arguments.next().unwrap_or_else(|| usage());
    if command != "serve" {
        return client(command, arguments.collect()).await;
    }
    let mut socket = None;
    let mut name = "atlas-swarm".to_owned();
    let mut bootstrap = None;
    let mut root_user: Option<UserId> = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--socket" => socket = Some(arguments.next().unwrap_or_else(|| usage()).into()),
            "--name" => name = arguments.next().unwrap_or_else(|| usage()),
            "--bootstrap" => {
                bootstrap = Some(iroh::EndpointAddr::new(
                    arguments.next().unwrap_or_else(|| usage()).parse()?,
                ))
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
    let root_user = root_user.unwrap_or_else(|| usage());
    let root_acl = PathAcl {
        readers: [root_user].into_iter().collect(),
        writers: [root_user].into_iter().collect(),
    };
    let swarm =
        Arc::new(Swarm::start(name, root_acl, bootstrap, Arc::new(MemoryStore::default())).await?);
    serve_daemon(
        &socket.unwrap_or(default_socket()?),
        LocalDaemon::new(swarm),
    )
    .await?;
    Ok(())
}

async fn client(command: String, arguments: Vec<String>) -> Result<(), Box<dyn std::error::Error>> {
    let mut socket: Option<PathBuf> = None;
    let mut positional = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--socket" => socket = Some(arguments.next().unwrap_or_else(|| usage()).into()),
            value if value.starts_with('-') => usage(),
            value => positional.push(value.to_owned()),
        }
    }
    let control = match socket {
        Some(socket) => connect_control(&socket).await?,
        None => autostart().await?,
    };
    match command.as_str() {
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
                let indent = "  ".repeat(path.as_str().split('/').count().saturating_sub(1));
                let kind = match entry.resource {
                    Some(PathResource::Service(service)) => format!("service {}", service.provider),
                    Some(PathResource::Repository(repository)) => {
                        format!("repository ({} endpoints)", repository.endpoints.len())
                    }
                    Some(PathResource::State(_)) => "state".into(),
                    None => "path".into(),
                };
                println!(
                    "{indent}{} [{kind}]",
                    path.as_str().rsplit('/').next().unwrap()
                );
            }
        }
        "info" => {
            if !positional.is_empty() {
                usage();
            }
            let info = control.info(()).await.map_err(std::io::Error::other)?;
            println!("swarm: {}", info.swarm_id);
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
