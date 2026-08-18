use atlas_daemon::ServeOptions;
use atlas_swarm::{SwarmPath, local::default_socket};

fn usage() -> ! {
    eprintln!(
        "usage: atlas-daemon serve [--root-user USER_ID] [--join-path PATH] [--socket PATH] [--name NAME] [--bootstrap ENDPOINT_ID]"
    );
    std::process::exit(2);
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    if arguments.next().as_deref() != Some("serve") {
        usage();
    }
    let mut socket = None;
    let mut name = "atlas-daemon".to_owned();
    let mut bootstrap = None;
    let mut join_path = None;
    let mut root_user = None;
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
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
    let root_user = match root_user {
        Some(user) => user,
        None => atlas_swarm::auth::UserSigner::discover().await?.user(),
    };
    atlas_daemon::serve(ServeOptions {
        socket: socket.unwrap_or(default_socket()?),
        name,
        root_user,
        bootstrap,
        join_path,
    })
    .await
}
