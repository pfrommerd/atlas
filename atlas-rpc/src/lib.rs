//! Tokio-native, bidirectional RPC over arbitrary message transports.

use std::any::Any;
use std::collections::HashMap;
use std::error::Error;
use std::fmt::{self, Display};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
pub use futures_util::StreamExt as RpcStreamExt;
use futures_util::{Sink, SinkExt, Stream};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

pub use atlas_rpc_derive::interface;
#[doc(hidden)]
pub use serde;

pub trait Transport<SinkItem, Item>:
    Stream<Item = Result<Item, <Self as Sink<SinkItem>>::Error>>
    + Sink<SinkItem, Error = Self::TransportError>
where
    <Self as Sink<SinkItem>>::Error: Error,
{
    type TransportError: Error + Send + Sync + 'static;
}

impl<T, SinkItem, Item, E> Transport<SinkItem, Item> for T
where
    T: ?Sized + Stream<Item = Result<Item, E>> + Sink<SinkItem, Error = E>,
    E: Error + Send + Sync + 'static,
{
    type TransportError = E;
}

#[doc(hidden)]
pub trait ErasedPayload: Send {
    fn json(&self) -> Result<Value, RpcError>;
    fn into_any(self: Box<Self>) -> Box<dyn Any + Send>;
}

struct TypedPayload<T>(T);

impl<T: Serialize + Send + 'static> ErasedPayload for TypedPayload<T> {
    fn json(&self) -> Result<Value, RpcError> {
        serde_json::to_value(&self.0).map_err(RpcError::internal)
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any + Send> {
        Box::new(self.0)
    }
}

/// A payload is typed while it remains in-process and becomes JSON only at a wire codec.
pub enum Payload {
    Typed(Box<dyn ErasedPayload>),
    Wire(Value),
}

impl Payload {
    pub fn new<T: Serialize + Send + 'static>(value: T) -> Self {
        Self::Typed(Box::new(TypedPayload(value)))
    }

    pub fn decode<T: DeserializeOwned + Send + 'static>(self) -> Result<T, RpcError> {
        match self {
            Self::Typed(value) => value
                .into_any()
                .downcast::<T>()
                .map(|value| *value)
                .map_err(|_| RpcError::internal("in-process RPC payload type mismatch")),
            Self::Wire(value) => serde_json::from_value(value).map_err(RpcError::invalid_params),
        }
    }

    fn into_json(self) -> Result<Value, RpcError> {
        match self {
            Self::Typed(value) => value.json(),
            Self::Wire(value) => Ok(value),
        }
    }
}

/// Runtime message representation. `InProcessTransport` moves these without serialization.
pub enum Envelope {
    Request {
        id: u64,
        method: String,
        params: Payload,
    },
    Notification {
        method: String,
        params: Payload,
    },
    StreamItem {
        id: u64,
        item: Payload,
    },
    Response {
        id: u64,
        result: Result<Payload, RpcError>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
    pub fn parse(error: impl Display) -> Self {
        Self::new(-32700, error.to_string())
    }
    pub fn invalid_request(error: impl Display) -> Self {
        Self::new(-32600, error.to_string())
    }
    pub fn method_not_found(method: &str) -> Self {
        Self::new(-32601, format!("method not found: {method}"))
    }
    pub fn invalid_params(error: impl Display) -> Self {
        Self::new(-32602, error.to_string())
    }
    pub fn internal(error: impl Display) -> Self {
        Self::new(-32603, error.to_string())
    }
    pub fn application<E: Serialize + Display>(error: E) -> Self {
        Self {
            code: -32000,
            message: error.to_string(),
            data: serde_json::to_value(error).ok(),
        }
    }
}

impl Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}
impl Error for RpcError {}

#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("RPC peer closed")]
    Closed,
    #[error("RPC error: {0}")]
    Rpc(#[from] RpcError),
}

type HandlerFuture = Pin<Box<dyn Future<Output = Result<Payload, RpcError>> + Send>>;
type NotificationFuture = Pin<Box<dyn Future<Output = Result<(), RpcError>> + Send>>;
type RequestHandler = Arc<dyn Fn(Payload, Peer, u64) -> HandlerFuture + Send + Sync>;
type NotificationHandler = Arc<dyn Fn(Payload, Peer) -> NotificationFuture + Send + Sync>;

struct Inner {
    outbound: mpsc::UnboundedSender<Envelope>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Pending>>,
    requests: Mutex<HashMap<String, RequestHandler>>,
    notifications: Mutex<HashMap<String, NotificationHandler>>,
}

enum Pending {
    Unary(oneshot::Sender<Result<Payload, CallError>>),
    Stream(mpsc::UnboundedSender<Result<Payload, CallError>>),
}

#[derive(Clone)]
pub struct Peer(Arc<Inner>);

impl Peer {
    pub fn new<T>(transport: T) -> Self
    where
        T: Transport<Envelope, Envelope> + Send + 'static,
    {
        let (outbound, mut outgoing) = mpsc::unbounded_channel();
        let peer = Self(Arc::new(Inner {
            outbound,
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            requests: Mutex::new(HashMap::new()),
            notifications: Mutex::new(HashMap::new()),
        }));
        let (mut sink, mut stream) = transport.split();
        tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                if sink.send(message).await.is_err() {
                    break;
                }
            }
        });
        let reader = peer.clone();
        tokio::spawn(async move {
            while let Some(message) = stream.next().await {
                match message {
                    Ok(message) => reader.receive(message),
                    Err(_) => break,
                }
            }
            reader.close();
        });
        peer
    }

    pub async fn call<Req, Res>(
        &self,
        method: impl Into<String>,
        request: Req,
    ) -> Result<Res, CallError>
    where
        Req: Serialize + Send + 'static,
        Res: DeserializeOwned + Send + 'static,
    {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.0
            .pending
            .lock()
            .unwrap()
            .insert(id, Pending::Unary(sender));
        self.send(Envelope::Request {
            id,
            method: method.into(),
            params: Payload::new(request),
        })?;
        let payload = receiver.await.map_err(|_| CallError::Closed)??;
        payload.decode().map_err(CallError::from)
    }

    pub fn notify<Req: Serialize + Send + 'static>(
        &self,
        method: impl Into<String>,
        params: Req,
    ) -> Result<(), CallError> {
        self.send(Envelope::Notification {
            method: method.into(),
            params: Payload::new(params),
        })
    }

    pub fn stream<Req, Item>(&self, method: impl Into<String>, request: Req) -> ClientStream<Item>
    where
        Req: Serialize + Send + 'static,
        Item: DeserializeOwned + Send + 'static,
    {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::unbounded_channel();
        self.0
            .pending
            .lock()
            .unwrap()
            .insert(id, Pending::Stream(sender));
        if let Err(error) = self.send(Envelope::Request {
            id,
            method: method.into(),
            params: Payload::new(request),
        }) {
            if let Some(Pending::Stream(sender)) = self.0.pending.lock().unwrap().remove(&id) {
                let _ = sender.send(Err(error));
            }
        }
        ClientStream {
            receiver,
            marker: std::marker::PhantomData,
        }
    }

    pub fn stream_item<Item: Serialize + Send + 'static>(
        &self,
        id: u64,
        item: Item,
    ) -> Result<(), CallError> {
        self.send(Envelope::StreamItem {
            id,
            item: Payload::new(item),
        })
    }

    pub fn register_request<F>(&self, method: impl Into<String>, handler: F)
    where
        F: Fn(Payload, Peer, u64) -> HandlerFuture + Send + Sync + 'static,
    {
        self.0
            .requests
            .lock()
            .unwrap()
            .insert(method.into(), Arc::new(handler));
    }

    pub fn register_notification<F>(&self, method: impl Into<String>, handler: F)
    where
        F: Fn(Payload, Peer) -> NotificationFuture + Send + Sync + 'static,
    {
        self.0
            .notifications
            .lock()
            .unwrap()
            .insert(method.into(), Arc::new(handler));
    }

    fn send(&self, message: Envelope) -> Result<(), CallError> {
        self.0.outbound.send(message).map_err(|_| CallError::Closed)
    }

    fn receive(&self, message: Envelope) {
        match message {
            Envelope::Response { id, result } => {
                if let Some(waiter) = self.0.pending.lock().unwrap().remove(&id) {
                    match waiter {
                        Pending::Unary(waiter) => {
                            let _ = waiter.send(result.map_err(CallError::from));
                        }
                        Pending::Stream(sender) => {
                            if let Err(error) = result {
                                let _ = sender.send(Err(CallError::Rpc(error)));
                            }
                        }
                    }
                }
            }
            Envelope::StreamItem { id, item } => {
                if let Some(Pending::Stream(sender)) = self.0.pending.lock().unwrap().get(&id) {
                    let _ = sender.send(Ok(item));
                }
            }
            Envelope::Request { id, method, params } => {
                let handler = self.0.requests.lock().unwrap().get(&method).cloned();
                let peer = self.clone();
                tokio::spawn(async move {
                    let result = match handler {
                        Some(handler) => handler(params, peer.clone(), id).await,
                        None => Err(RpcError::method_not_found(&method)),
                    };
                    let _ = peer.send(Envelope::Response { id, result });
                });
            }
            Envelope::Notification { method, params } => {
                if let Some(handler) = self.0.notifications.lock().unwrap().get(&method).cloned() {
                    let peer = self.clone();
                    tokio::spawn(async move {
                        let _ = handler(params, peer).await;
                    });
                }
            }
        }
    }

    fn close(&self) {
        for (_, waiter) in self.0.pending.lock().unwrap().drain() {
            match waiter {
                Pending::Unary(waiter) => {
                    let _ = waiter.send(Err(CallError::Closed));
                }
                Pending::Stream(sender) => {
                    let _ = sender.send(Err(CallError::Closed));
                }
            }
        }
    }
}

pub struct ServerStream<T>(Pin<Box<dyn Stream<Item = T> + Send>>);
impl<T> ServerStream<T> {
    pub fn new(stream: impl Stream<Item = T> + Send + 'static) -> Self {
        Self(Box::pin(stream))
    }
}
impl<T> Stream for ServerStream<T> {
    type Item = T;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        self.0.as_mut().poll_next(cx)
    }
}

pub struct ClientStream<T> {
    receiver: mpsc::UnboundedReceiver<Result<Payload, CallError>>,
    marker: std::marker::PhantomData<T>,
}
impl<T> Unpin for ClientStream<T> {}
impl<T: DeserializeOwned + Send + 'static> Stream for ClientStream<T> {
    type Item = Result<T, CallError>;
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.get_mut().receiver.poll_recv(cx) {
            Poll::Ready(Some(Ok(payload))) => {
                Poll::Ready(Some(payload.decode().map_err(CallError::from)))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(error))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub trait RpcClient: Clone + Send + Sync + 'static {
    fn from_peer(peer: Peer) -> Self;
}

pub struct RpcContext<C: RpcClient> {
    client: C,
}
impl<C: RpcClient> RpcContext<C> {
    pub fn from_peer(peer: Peer) -> Self {
        Self {
            client: C::from_peer(peer),
        }
    }
    pub fn client(&self) -> &C {
        &self.client
    }
}

#[derive(Debug, thiserror::Error)]
#[error("in-process RPC endpoint closed")]
pub struct InProcessError;

pub struct InProcessTransport {
    sender: mpsc::UnboundedSender<Envelope>,
    receiver: mpsc::UnboundedReceiver<Envelope>,
}
impl InProcessTransport {
    pub fn pair() -> (Self, Self) {
        let (left_send, left_recv) = mpsc::unbounded_channel();
        let (right_send, right_recv) = mpsc::unbounded_channel();
        (
            Self {
                sender: left_send,
                receiver: right_recv,
            },
            Self {
                sender: right_send,
                receiver: left_recv,
            },
        )
    }
}
impl Stream for InProcessTransport {
    type Item = Result<Envelope, InProcessError>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.receiver.poll_recv(cx).map(|item| item.map(Ok))
    }
}
impl Sink<Envelope> for InProcessTransport {
    type Error = InProcessError;
    fn poll_ready(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn start_send(self: Pin<&mut Self>, item: Envelope) -> Result<(), Self::Error> {
        self.sender.send(item).map_err(|_| InProcessError)
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError<E: Error + Send + Sync + 'static> {
    #[error(transparent)]
    Transport(E),
    #[error(transparent)]
    Rpc(#[from] RpcError),
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum WireEnvelope {
    Request {
        jsonrpc: String,
        id: u64,
        method: String,
        params: Value,
    },
    StreamItem {
        jsonrpc: String,
        method: String,
        params: StreamWireItem,
    },
    Notification {
        jsonrpc: String,
        method: String,
        params: Value,
    },
    Success {
        jsonrpc: String,
        id: u64,
        result: Value,
    },
    Failure {
        jsonrpc: String,
        id: u64,
        error: RpcError,
    },
}
#[derive(Serialize, Deserialize)]
struct StreamWireItem {
    id: u64,
    item: Value,
}
impl TryFrom<Envelope> for WireEnvelope {
    type Error = RpcError;
    fn try_from(value: Envelope) -> Result<Self, Self::Error> {
        Ok(match value {
            Envelope::Request { id, method, params } => Self::Request {
                jsonrpc: "2.0".into(),
                id,
                method,
                params: params.into_json()?,
            },
            Envelope::Notification { method, params } => Self::Notification {
                jsonrpc: "2.0".into(),
                method,
                params: params.into_json()?,
            },
            Envelope::StreamItem { id, item } => Self::StreamItem {
                jsonrpc: "2.0".into(),
                method: "$/atlas-rpc/stream".into(),
                params: StreamWireItem {
                    id,
                    item: item.into_json()?,
                },
            },
            Envelope::Response {
                id,
                result: Ok(payload),
            } => Self::Success {
                jsonrpc: "2.0".into(),
                id,
                result: payload.into_json()?,
            },
            Envelope::Response {
                id,
                result: Err(error),
            } => Self::Failure {
                jsonrpc: "2.0".into(),
                id,
                error,
            },
        })
    }
}
impl TryFrom<WireEnvelope> for Envelope {
    type Error = RpcError;
    fn try_from(value: WireEnvelope) -> Result<Self, Self::Error> {
        match value {
            WireEnvelope::Request {
                jsonrpc,
                id,
                method,
                params,
            } if jsonrpc == "2.0" => Ok(Self::Request {
                id,
                method,
                params: Payload::Wire(params),
            }),
            WireEnvelope::Notification {
                jsonrpc,
                method,
                params,
            } if jsonrpc == "2.0" => Ok(Self::Notification {
                method,
                params: Payload::Wire(params),
            }),
            WireEnvelope::StreamItem {
                jsonrpc,
                method,
                params,
            } if jsonrpc == "2.0" && method == "$/atlas-rpc/stream" => Ok(Self::StreamItem {
                id: params.id,
                item: Payload::Wire(params.item),
            }),
            WireEnvelope::Success {
                jsonrpc,
                id,
                result,
            } if jsonrpc == "2.0" => Ok(Self::Response {
                id,
                result: Ok(Payload::Wire(result)),
            }),
            WireEnvelope::Failure { jsonrpc, id, error } if jsonrpc == "2.0" => {
                Ok(Self::Response {
                    id,
                    result: Err(error),
                })
            }
            _ => Err(RpcError::invalid_request("jsonrpc must be 2.0")),
        }
    }
}

pub struct JsonTransport<T>(pub T);
impl<T, E> Stream for JsonTransport<T>
where
    T: Stream<Item = Result<String, E>> + Unpin,
    E: Error + Send + Sync + 'static,
{
    type Item = Result<Envelope, CodecError<E>>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.0).poll_next(cx) {
            Poll::Ready(Some(Ok(line))) => Poll::Ready(Some(
                serde_json::from_str::<WireEnvelope>(&line)
                    .map_err(|e| CodecError::Rpc(RpcError::parse(e)))
                    .and_then(|m| m.try_into().map_err(CodecError::Rpc)),
            )),
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(CodecError::Transport(e)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
impl<T, E> Sink<Envelope> for JsonTransport<T>
where
    T: Sink<String, Error = E> + Unpin,
    E: Error + Send + Sync + 'static,
{
    type Error = CodecError<E>;
    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0)
            .poll_ready(cx)
            .map_err(CodecError::Transport)
    }
    fn start_send(mut self: Pin<&mut Self>, item: Envelope) -> Result<(), Self::Error> {
        let wire = WireEnvelope::try_from(item)?;
        Pin::new(&mut self.0)
            .start_send(
                serde_json::to_string(&wire).map_err(|e| CodecError::Rpc(RpcError::internal(e)))?,
            )
            .map_err(CodecError::Transport)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0)
            .poll_flush(cx)
            .map_err(CodecError::Transport)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0)
            .poll_close(cx)
            .map_err(CodecError::Transport)
    }
}

pub struct CborTransport<T>(pub T);
impl<T, E> Stream for CborTransport<T>
where
    T: Stream<Item = Result<Bytes, E>> + Unpin,
    E: Error + Send + Sync + 'static,
{
    type Item = Result<Envelope, CodecError<E>>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.0).poll_next(cx) {
            Poll::Ready(Some(Ok(bytes))) => Poll::Ready(Some(
                serde_cbor::from_slice::<WireEnvelope>(&bytes)
                    .map_err(|e| CodecError::Rpc(RpcError::parse(e)))
                    .and_then(|m| m.try_into().map_err(CodecError::Rpc)),
            )),
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(CodecError::Transport(e)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}
impl<T, E> Sink<Envelope> for CborTransport<T>
where
    T: Sink<Bytes, Error = E> + Unpin,
    E: Error + Send + Sync + 'static,
{
    type Error = CodecError<E>;
    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0)
            .poll_ready(cx)
            .map_err(CodecError::Transport)
    }
    fn start_send(mut self: Pin<&mut Self>, item: Envelope) -> Result<(), Self::Error> {
        let bytes = serde_cbor::to_vec(&WireEnvelope::try_from(item)?)
            .map_err(|e| CodecError::Rpc(RpcError::internal(e)))?;
        Pin::new(&mut self.0)
            .start_send(Bytes::from(bytes))
            .map_err(CodecError::Transport)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0)
            .poll_flush(cx)
            .map_err(CodecError::Transport)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0)
            .poll_close(cx)
            .map_err(CodecError::Transport)
    }
}
