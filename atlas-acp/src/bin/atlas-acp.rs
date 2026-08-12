use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use atlas_acp::host::{Config, Host};
use atlas_swarm::{
    auth::UserSigner,
    local::{
        connect_control, default_socket as default_swarm_socket, serve_local_registered,
        CommitHistoryRequest, RegisterLocalService, StateSelector, StateSnapshot,
    },
    Commit, PathOperation, ServiceRecord, SwarmOperation, SwarmPath,
};
use futures_util::StreamExt;
use tokio::{net::UnixListener, sync::RwLock};

fn config_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("atlas/config.toml"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config/atlas/config.toml"))
}

fn default_service_socket() -> io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    Ok(runtime.join("atlas/acp.sock"))
}

async fn bind_socket(socket: &Path) -> io::Result<UnixListener> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        match tokio::net::UnixStream::connect(socket).await {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "atlas-acp already owns the socket",
                ))
            }
            Err(_) => std::fs::remove_file(socket)?,
        }
    }
    let listener = UnixListener::bind(socket)?;
    #[cfg(unix)]
    std::fs::set_permissions(socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(listener)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let source = std::fs::read_to_string(config_path()?)?;
    let config: Config = toml::from_str(&source)?;
    let service_path = SwarmPath::new(config.swarm.service_path.clone())
        .ok_or("swarm.service_path must be an absolute swarm path")?;
    let (host, agents) = Host::from_config(config)?;
    host.start_children(agents);

    let signer = UserSigner::discover().await?;
    let control = connect_control(&default_swarm_socket()?).await?;
    let user = signer.user();
    let socket = default_service_socket()?;
    let listener = bind_socket(&socket).await?;
    let provider = control.endpoint_id(()).await.map_err(io::Error::other)?;
    let history = control
        .commit_history(CommitHistoryRequest {
            starts: Vec::new(),
            depth: 0,
        })
        .await
        .map_err(io::Error::other)?;
    let mut commit = Commit::new_unsigned(
        history
            .commits
            .into_iter()
            .map(|commit| commit.id)
            .collect(),
        provider,
        user,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as u64,
        SwarmOperation::Path(PathOperation::DefineService {
            path: service_path.clone(),
            service: ServiceRecord {
                provider,
                allowed_users: [user].into_iter().collect(),
            },
        }),
    );
    commit.user_signature = signer.sign(&commit.signing_bytes()).await?;
    control
        .register_local_service(RegisterLocalService { commit, socket })
        .await
        .map_err(io::Error::other)?;
    let (snapshot, mut updates) = control
        .watch(StateSelector::Path {
            path: service_path.clone(),
        })
        .await
        .map_err(io::Error::other)?;
    let StateSnapshot::Path(state) = snapshot else {
        unreachable!()
    };
    let view = Arc::new(RwLock::new(state));
    let watched_view = view.clone();
    tokio::spawn(async move {
        while let Some(Ok(change)) = updates.next().await {
            if let StateSnapshot::Path(state) = change.snapshot {
                *watched_view.write().await = state;
            }
        }
    });
    let result = serve_local_registered(listener, service_path, view, move |peer| {
        host.register(peer)
    })
    .await;
    drop(control); // Retain registration until the local host stops serving.
    result?;
    Ok(())
}
