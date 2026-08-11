use std::{env, process, sync::Arc};

use atlas_swarm::{
    local::{default_socket, serve_daemon, LocalDaemon},
    MemoryStore, Swarm,
};

fn usage() -> ! {
    eprintln!("usage: atlas-swarm serve [--socket PATH] [--name NAME] [--bootstrap ENDPOINT_ID]");
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
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--socket" => socket = Some(arguments.next().unwrap_or_else(|| usage()).into()),
            "--name" => name = arguments.next().unwrap_or_else(|| usage()),
            "--bootstrap" => {
                bootstrap = Some(iroh::EndpointAddr::new(
                    arguments.next().unwrap_or_else(|| usage()).parse()?,
                ))
            }
            _ => usage(),
        }
    }
    let swarm = Arc::new(Swarm::start(name, bootstrap, Arc::new(MemoryStore::default())).await?);
    serve_daemon(
        &socket.unwrap_or(default_socket()?),
        LocalDaemon::new(swarm),
    )
    .await?;
    Ok(())
}
