//! Unix-socket control and service helpers for a local swarm daemon.

use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use atlas_rpc::{interface, CborTransport, Peer};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::{Sink, Stream};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::RwLock,
};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use uuid::Uuid;

use crate::{
    auth_bytes, can_read, PathOperation, PathResource, SignedPathOperation, SignedUserMetadata,
    Swarm, SwarmError, SwarmPath, SwarmView, UserId, UserSignature,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisterLocalService {
    pub operation: SignedPathOperation,
    pub socket: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResolveServiceRequest {
    pub path: SwarmPath,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceResolution {
    pub provider: iroh::EndpointId,
    pub endpoint_addr: iroh::EndpointAddr,
    pub local_socket: Option<PathBuf>,
}

#[interface]
pub trait SwarmControl {
    async fn endpoint_id(&self, request: ()) -> Result<iroh::EndpointId, String>;
    async fn initialize(&self, operation: SignedPathOperation) -> Result<(), String>;
    async fn submit_path(&self, operation: SignedPathOperation) -> Result<(), String>;
    async fn submit_user_metadata(&self, metadata: SignedUserMetadata) -> Result<(), String>;
    async fn register_local_service(&self, request: RegisterLocalService) -> Result<(), String>;
    async fn unregister_local_service(&self, request: ResolveServiceRequest) -> Result<(), String>;
    async fn resolve_service(
        &self,
        request: ResolveServiceRequest,
    ) -> Result<ServiceResolution, String>;
    async fn view(&self, request: ()) -> Result<SwarmView, String>;
}

#[derive(Clone)]
pub struct LocalDaemon {
    swarm: Arc<Swarm>,
    services: Arc<RwLock<BTreeMap<SwarmPath, LocalService>>>,
    connection: Option<Uuid>,
}

#[derive(Clone)]
struct LocalService {
    socket: PathBuf,
    connection: Option<Uuid>,
}

impl LocalDaemon {
    pub fn new(swarm: Arc<Swarm>) -> Self {
        Self {
            swarm,
            services: Arc::new(RwLock::new(BTreeMap::new())),
            connection: None,
        }
    }

    fn for_connection(&self) -> Self {
        Self {
            swarm: self.swarm.clone(),
            services: self.services.clone(),
            connection: Some(Uuid::new_v4()),
        }
    }

    async fn remove_connection_services(&self) {
        let Some(connection) = self.connection else {
            return;
        };
        self.services
            .write()
            .await
            .retain(|_, service| service.connection != Some(connection));
    }
}

impl SwarmControl for LocalDaemon {
    async fn endpoint_id(&self, _: ()) -> Result<iroh::EndpointId, String> {
        Ok(self.swarm.endpoint_id())
    }
    async fn initialize(&self, operation: SignedPathOperation) -> Result<(), String> {
        self.swarm
            .initialize_path_tree(operation)
            .await
            .map_err(|error| error.to_string())
    }

    async fn submit_path(&self, operation: SignedPathOperation) -> Result<(), String> {
        self.swarm
            .submit_path_operation(operation)
            .await
            .map_err(|error| error.to_string())
    }

    async fn submit_user_metadata(&self, metadata: SignedUserMetadata) -> Result<(), String> {
        self.swarm
            .submit_user_metadata(metadata)
            .await
            .map_err(|error| error.to_string())
    }

    async fn register_local_service(&self, request: RegisterLocalService) -> Result<(), String> {
        let PathOperation::DefineService { path, service } = &request.operation.operation else {
            return Err("local registration requires a service definition".into());
        };
        if service.provider != self.swarm.endpoint_id() {
            return Err("local service provider must be this daemon endpoint".into());
        }
        if !request.socket.is_absolute() {
            return Err("local service socket must be absolute".into());
        }
        let path = path.clone();
        self.swarm
            .submit_path_operation(request.operation)
            .await
            .map_err(|error| error.to_string())?;
        self.services.write().await.insert(
            path,
            LocalService {
                socket: request.socket,
                connection: self.connection,
            },
        );
        Ok(())
    }

    async fn unregister_local_service(&self, request: ResolveServiceRequest) -> Result<(), String> {
        self.services.write().await.remove(&request.path);
        Ok(())
    }

    async fn resolve_service(
        &self,
        request: ResolveServiceRequest,
    ) -> Result<ServiceResolution, String> {
        let view = self.swarm.view().await;
        let service = match view
            .paths
            .get(&request.path)
            .and_then(|entry| entry.resource.as_ref())
        {
            Some(PathResource::Service(service)) => service,
            _ => return Err(format!("service is unavailable: {}", request.path.as_str())),
        };
        let endpoint_addr = view
            .membership
            .nodes
            .values()
            .find(|node| node.endpoint_id == service.provider)
            .map(|node| node.endpoint_addr.clone())
            .ok_or_else(|| format!("service is unavailable: {}", request.path.as_str()))?;
        let local_socket = if service.provider == self.swarm.endpoint_id() {
            self.services
                .read()
                .await
                .get(&request.path)
                .map(|service| service.socket.clone())
        } else {
            None
        };
        Ok(ServiceResolution {
            provider: service.provider,
            endpoint_addr,
            local_socket,
        })
    }

    async fn view(&self, _: ()) -> Result<SwarmView, String> {
        Ok(self.swarm.view().await)
    }
}

pub fn default_socket() -> io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR is not set"))?;
    Ok(runtime.join("atlas").join("swarm.sock"))
}

pub async fn serve_daemon(socket: &Path, daemon: LocalDaemon) -> io::Result<()> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if socket.exists() {
        match UnixStream::connect(socket).await {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "atlas-swarm daemon already owns the socket",
                ))
            }
            Err(_) => std::fs::remove_file(socket)?,
        }
    }
    let listener = UnixListener::bind(socket)?;
    #[cfg(unix)]
    {
        std::fs::set_permissions(socket, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    }
    loop {
        let (stream, _) = listener.accept().await?;
        let daemon = daemon.for_connection();
        tokio::spawn(async move {
            let peer = unix_peer(stream);
            peer.register::<SwarmControlHandle, _>(daemon.clone());
            peer.closed().await;
            daemon.remove_connection_services().await;
        });
    }
}

pub async fn connect_control(socket: &Path) -> io::Result<SwarmControlHandle> {
    let stream = UnixStream::connect(socket).await?;
    Ok(SwarmControlHandle::new(unix_peer(stream)))
}

#[derive(Serialize, Deserialize)]
struct ServiceHello {
    path: SwarmPath,
    user: UserId,
}
#[derive(Serialize, Deserialize)]
struct ServiceChallenge {
    nonce: [u8; 32],
}
#[derive(Serialize, Deserialize)]
struct ServiceProof {
    signature: UserSignature,
}

pub async fn connect_local_service(
    socket: &Path,
    path: &SwarmPath,
    key: &SigningKey,
) -> Result<Peer, SwarmError> {
    let mut stream = UnixStream::connect(socket).await?;
    write_frame(
        &mut stream,
        &ServiceHello {
            path: path.clone(),
            user: UserId::from_signing_key(key),
        },
    )
    .await?;
    let challenge: ServiceChallenge = read_frame(&mut stream).await?;
    let signature = UserSignature::Ed25519(key.sign(&auth_bytes(path, &challenge.nonce)).to_bytes().to_vec());
    write_frame(&mut stream, &ServiceProof { signature }).await?;
    let accepted: bool = read_frame(&mut stream).await?;
    if !accepted {
        return Err(SwarmError::AuthenticationFailed);
    }
    Ok(unix_peer(stream))
}

/// Connects with an `ssh-ed25519` identity held by the local SSH agent.
pub async fn connect_local_service_with_agent(
    socket: &Path,
    path: &SwarmPath,
    signer: &crate::auth::UserSigner,
) -> Result<Peer, SwarmError> {
    let mut stream = UnixStream::connect(socket).await?;
    write_frame(&mut stream, &ServiceHello { path: path.clone(), user: signer.user() }).await?;
    let challenge: ServiceChallenge = read_frame(&mut stream).await?;
    let signature = signer.sign(&auth_bytes(path, &challenge.nonce)).await?;
    write_frame(&mut stream, &ServiceProof { signature }).await?;
    let accepted: bool = read_frame(&mut stream).await?;
    if !accepted { return Err(SwarmError::AuthenticationFailed); }
    Ok(unix_peer(stream))
}

pub async fn accept_local_service(
    stream: &mut UnixStream,
    view: &SwarmView,
) -> Result<SwarmPath, SwarmError> {
    let hello: ServiceHello = read_frame(stream).await?;
    let mut nonce = [0; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
    write_frame(stream, &ServiceChallenge { nonce }).await?;
    let proof: ServiceProof = read_frame(stream).await?;
    let allowed = can_read(view, &hello.path, hello.user)
        && matches!(view.paths.get(&hello.path).and_then(|entry| entry.resource.as_ref()), Some(PathResource::Service(service)) if service.allowed_users.contains(&hello.user) && proof.signature.verify(hello.user, &auth_bytes(&hello.path, &nonce)));
    write_frame(stream, &allowed).await?;
    if !allowed {
        return Err(SwarmError::AuthenticationFailed);
    }
    Ok(hello.path)
}

pub async fn serve_local<H, S>(
    listener: UnixListener,
    path: SwarmPath,
    view: Arc<RwLock<SwarmView>>,
    service: S,
) -> io::Result<()>
where
    H: atlas_rpc::Service<S> + Send + Sync + 'static,
    S: Clone + Send + Sync + 'static,
{
    loop {
        let (mut stream, _) = listener.accept().await?;
        let view = view.clone();
        let path = path.clone();
        let service = service.clone();
        tokio::spawn(async move {
            if matches!(accept_local_service(&mut stream, &*view.read().await).await, Ok(accepted_path) if accepted_path == path)
            {
                let peer = unix_peer(stream);
                H::register(service, &peer);
                peer.closed().await;
            }
        });
    }
}

/// Serves a local swarm service whose RPC registration needs multiple interfaces.
pub async fn serve_local_registered<F>(
    listener: UnixListener,
    path: SwarmPath,
    view: Arc<RwLock<SwarmView>>,
    register: F,
) -> io::Result<()>
where
    F: Fn(&Peer) + Clone + Send + Sync + 'static,
{
    loop {
        let (mut stream, _) = listener.accept().await?;
        let view = view.clone();
        let path = path.clone();
        let register = register.clone();
        tokio::spawn(async move {
            if matches!(accept_local_service(&mut stream, &*view.read().await).await, Ok(accepted_path) if accepted_path == path) {
                let peer = unix_peer(stream);
                register(&peer);
                peer.closed().await;
            }
        });
    }
}

fn unix_peer(stream: UnixStream) -> Peer {
    Peer::new(CborTransport(UnixTransport(Framed::new(
        stream,
        LengthDelimitedCodec::new(),
    ))))
}

struct UnixTransport(Framed<UnixStream, LengthDelimitedCodec>);

impl Stream for UnixTransport {
    type Item = Result<bytes::Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.0)
            .poll_next(cx)
            .map(|item| item.map(|item| item.map(|bytes| bytes.freeze())))
    }
}

impl Sink<bytes::Bytes> for UnixTransport {
    type Error = io::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0).poll_ready(cx)
    }
    fn start_send(mut self: Pin<&mut Self>, item: bytes::Bytes) -> Result<(), Self::Error> {
        Pin::new(&mut self.0).start_send(item.into())
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0).poll_close(cx)
    }
}

async fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<(), SwarmError> {
    let bytes = serde_cbor::to_vec(value).map_err(|error| SwarmError::Iroh(Box::new(error)))?;
    stream
        .write_all(
            &u32::try_from(bytes.len())
                .map_err(|_| SwarmError::AuthenticationFailed)?
                .to_be_bytes(),
        )
        .await?;
    stream.write_all(&bytes).await?;
    Ok(())
}

async fn read_frame<T: for<'de> Deserialize<'de>>(
    stream: &mut UnixStream,
) -> Result<T, SwarmError> {
    let mut length = [0; 4];
    stream.read_exact(&mut length).await?;
    let mut bytes = vec![0; u32::from_be_bytes(length) as usize];
    stream.read_exact(&mut bytes).await?;
    serde_cbor::from_slice(&bytes).map_err(|_| SwarmError::AuthenticationFailed)
}
