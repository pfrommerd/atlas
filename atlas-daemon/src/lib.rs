//! Unified persistent daemon for the swarm node and Atlas agent backends.

use atlas_agent::{BackendId, Multiplexer};
use atlas_swarm::{
    Commit, PathAcl, PathOperation, RedbStore, ServiceRecord, Swarm, SwarmOperation, SwarmPath,
    UserId,
    auth::UserSigner,
    local::{
        LocalDaemon, SwarmControlHandle, autostart_at_with_executable, default_socket,
        reset_daemon, serve_daemon,
    },
};
use serde::Deserialize;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_SERVICE_PREFIX: &str = "/atlas";

#[derive(Debug, Deserialize)]
pub struct Config {
    pub daemon: DaemonConfig,
    #[serde(alias = "backends")]
    pub agents: HashMap<String, AgentConfig>,
}

#[derive(Debug, Deserialize)]
pub struct DaemonConfig {
    #[serde(alias = "default_backend")]
    pub default_agent: String,
    /// A single redb file for both metadata and repository objects. When set,
    /// this takes precedence over the two separate database paths.
    #[serde(default)]
    pub database: Option<PathBuf>,
    #[serde(default)]
    pub swarm_database: Option<PathBuf>,
    #[serde(default)]
    pub repository_database: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AgentConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

pub struct ServeOptions {
    pub socket: PathBuf,
    pub name: String,
    pub root_user: UserId,
    pub bootstrap: Option<iroh::EndpointAddr>,
    pub join_path: Option<SwarmPath>,
}

pub fn config_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("atlas/config.toml"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config/atlas/config.toml"))
}

pub fn data_path() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(path).join("atlas"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is not set"))?;
    Ok(PathBuf::from(home).join(".local/share/atlas"))
}

pub async fn connect_or_start(executable: &Path, reset: bool) -> io::Result<SwarmControlHandle> {
    let socket = default_socket()?;
    if reset {
        reset_daemon(&socket).await?;
    }
    autostart_at_with_executable(&socket, false, executable).await
}

pub async fn serve(options: ServeOptions) -> Result<(), Box<dyn std::error::Error>> {
    let source = std::fs::read_to_string(config_path()?)?;
    let config: Config = toml::from_str(&source)?;
    if config.agents.is_empty() || !config.agents.contains_key(&config.daemon.default_agent) {
        return Err("daemon.default_agent must name a configured agent".into());
    }
    let signer = UserSigner::discover().await?;
    if options.bootstrap.is_none() && options.root_user != signer.user() {
        return Err("--root-user must be the local SSH identity".into());
    }
    let root_acl = PathAcl {
        readers: [options.root_user].into_iter().collect(),
        writers: [options.root_user].into_iter().collect(),
    };
    let default_data_path = data_path()?;
    std::fs::create_dir_all(&default_data_path)?;
    let (swarm_store, repository_database) = if let Some(path) = config.daemon.database.as_ref() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        RedbStore::open_with_repository(path)?
    } else {
        let swarm_path = config
            .daemon
            .swarm_database
            .clone()
            .unwrap_or_else(|| default_data_path.join("swarm.redb"));
        let repository_path = config
            .daemon
            .repository_database
            .clone()
            .unwrap_or_else(|| default_data_path.join("repositories.redb"));
        if let Some(parent) = swarm_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if let Some(parent) = repository_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        (
            RedbStore::open(swarm_path)?,
            atlas_swarm::repository::RepositoryDatabase::open(repository_path)?,
        )
    };
    let swarm = Arc::new(
        Swarm::start_with_repository(
            options.name,
            root_acl.clone(),
            options.bootstrap.clone(),
            Arc::new(swarm_store),
            Some(repository_database.clone()),
        )
        .await?,
    );
    swarm.start_repository_replication_worker();
    let join_path = options.join_path.unwrap_or_else(|| {
        let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "localhost".into());
        SwarmPath::new(format!("/nodes/{host}")).expect("hostname produces a valid path")
    });
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis() as u64;
    if options.bootstrap.is_none()
        && swarm
            .store()
            .commits()
            .await
            .map_err(io::Error::other)?
            .is_empty()
    {
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
            .map_err(io::Error::other)?
            .into_iter()
            .map(|commit| commit.id)
            .collect(),
        swarm.endpoint_id(),
        signer.user(),
        now,
        SwarmOperation::Path(PathOperation::NodeJoin {
            path: join_path.clone(),
            node: atlas_swarm::NodeRecord {
                name: swarm.node_name().to_owned(),
                endpoint_id: swarm.endpoint_id(),
                endpoint_addr: swarm.endpoint_addr(),
                encryption_key: swarm.encryption_public_key(),
                coordinate: swarm.node_coordinate(),
            },
        }),
    );
    node_join.user_signature = signer.sign(&node_join.signing_bytes()).await?;
    swarm.submit_commit(node_join).await?;

    let node_leaf = join_path
        .as_str()
        .rsplit('/')
        .next()
        .filter(|leaf| !leaf.is_empty())
        .ok_or("the swarm root cannot be an Atlas node")?;
    let service_path = SwarmPath::new(format!("{DEFAULT_SERVICE_PREFIX}/{node_leaf}"))
        .expect("node leaf is valid in a service path");
    let multiplexer = Multiplexer::new(BackendId(config.daemon.default_agent));
    let registered = multiplexer.clone();
    swarm
        .register_rpc_service(service_path.clone(), move |peer| registered.register(&peer))
        .await;
    let mut service_commit = Commit::new_unsigned(
        swarm
            .store()
            .commits()
            .await
            .map_err(io::Error::other)?
            .into_iter()
            .map(|commit| commit.id)
            .collect(),
        swarm.endpoint_id(),
        signer.user(),
        now,
        SwarmOperation::Path(PathOperation::DefineService {
            path: service_path.clone(),
            service: ServiceRecord {
                provider: swarm.endpoint_id(),
                endpoint_addr: Some(swarm.endpoint_addr()),
                allowed_users: [signer.user()].into_iter().collect(),
            },
        }),
    );
    service_commit.user_signature = signer.sign(&service_commit.signing_bytes()).await?;
    swarm.submit_commit(service_commit).await?;

    let mut supervisors = Vec::new();
    for (name, agent) in config.agents {
        let multiplexer = multiplexer.clone();
        supervisors.push(tokio::spawn(async move {
            let backend = BackendId(name);
            let queue_store = atlas_acp::api_bridge::AcpQueueStore::default();
            let mut delay = Duration::from_millis(250);
            loop {
                match atlas_acp::api_bridge::spawn_with_queue_store(
                    backend.clone(),
                    &agent.command,
                    &agent.args,
                    queue_store.clone(),
                )
                .await
                {
                    Ok(mut spawned) => {
                        multiplexer.add_backend_service(backend.clone(), spawned.bridge.clone());
                        delay = Duration::from_millis(250);
                        let message = match spawned.child.wait().await {
                            Ok(status) => format!("ACP backend exited with {status}"),
                            Err(error) => format!("ACP backend wait failed: {error}"),
                        };
                        queue_store.pause_all();
                        multiplexer.remove_backend(&backend, message);
                    }
                    Err(error) => multiplexer
                        .remove_backend(&backend, format!("ACP backend failed to start: {error}")),
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(30));
            }
        }));
    }

    let result = serve_daemon(
        &options.socket,
        LocalDaemon::new(swarm.clone(), repository_database),
    )
    .await;
    for task in supervisors {
        task.abort();
    }
    swarm.unregister_rpc_service(&service_path).await;
    result?;
    Ok(())
}
