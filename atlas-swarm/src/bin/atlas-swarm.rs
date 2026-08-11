use std::{env, process, sync::Arc};

use atlas_swarm::{
    local::{default_socket, serve_daemon, LocalDaemon},
    MemoryStore, PathAcl, Swarm, UserId,
};

fn usage() -> ! {
    eprintln!("usage: atlas-swarm serve --root-user USER_ID [--socket PATH] [--name NAME] [--bootstrap ENDPOINT_ID]");
    process::exit(2);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args().skip(1);
    if arguments.next().as_deref() != Some("serve") {
        usage();
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
            "--root-user" => root_user = Some(arguments.next().unwrap_or_else(|| usage()).parse().unwrap_or_else(|_| usage())),
            _ => usage(),
        }
    }
    let root_user = root_user.unwrap_or_else(|| usage());
    let root_acl = PathAcl { readers: [root_user].into_iter().collect(), writers: [root_user].into_iter().collect() };
    let swarm = Arc::new(Swarm::start(name, root_acl, bootstrap, Arc::new(MemoryStore::default())).await?);
    serve_daemon(
        &socket.unwrap_or(default_socket()?),
        LocalDaemon::new(swarm),
    )
    .await?;
    Ok(())
}
