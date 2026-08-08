use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use atlas_rpc::{
    interface, InProcessTransport, IntoHandle, JsonTransport, Peer, RpcContext, Stream,
};
use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::duplex;
use tokio_util::codec::{Framed, LinesCodec};

#[derive(Debug, Serialize)]
struct DemoError;
impl fmt::Display for DemoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("demo failure")
    }
}

#[interface]
trait Callback {
    async fn acknowledge(&self, request: String) -> Result<(), DemoError>;
}

#[interface]
trait Service {
    async fn echo(&self, request: String) -> Result<String, DemoError>;

    #[rpc(payload)]
    async fn raw_echo(&self, value: String) -> Result<String, DemoError>;

    async fn join(
        &self,
        left: String,
        #[serde(rename = "rightValue")] right: String,
    ) -> Result<String, DemoError>;

    #[rpc(notification)]
    async fn event(&self, request: String) -> Result<(), DemoError>;

    #[rpc(notification)]
    async fn direct_event(&self, kind: String, value: u64) -> Result<(), DemoError>;

    async fn callback(
        &self,
        #[rpc(context)] context: RpcContext<CallbackHandle>,
        request: String,
    ) -> Result<(), DemoError>;

    async fn direct_callback(
        &self,
        #[rpc(context)] context: RpcContext<CallbackHandle>,
        message: String,
        count: usize,
    ) -> Result<(), DemoError>;

    #[rpc(stream)]
    async fn numbers(&self, request: ()) -> Result<Stream<u64>, DemoError>;
}

struct CallbackService(Arc<AtomicUsize>);
impl Callback for CallbackService {
    async fn acknowledge(&self, request: String) -> Result<(), DemoError> {
        assert!(matches!(request.as_str(), "ack" | "direct:2"));
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct EchoService(Arc<AtomicUsize>);
struct StreamDrop(Arc<AtomicUsize>);
impl Drop for StreamDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl Service for EchoService {
    async fn echo(&self, request: String) -> Result<String, DemoError> {
        Ok(request)
    }
    async fn raw_echo(&self, value: String) -> Result<String, DemoError> {
        Ok(value)
    }
    async fn join(&self, left: String, right: String) -> Result<String, DemoError> {
        Ok(format!("{left}:{right}"))
    }
    async fn event(&self, request: String) -> Result<(), DemoError> {
        assert_eq!(request, "event");
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn direct_event(&self, kind: String, value: u64) -> Result<(), DemoError> {
        assert_eq!(kind, "direct");
        assert_eq!(value, 2);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn callback(
        &self,
        context: RpcContext<CallbackHandle>,
        request: String,
    ) -> Result<(), DemoError> {
        context
            .handle()
            .acknowledge(request)
            .await
            .map_err(|_| DemoError)?;
        Ok(())
    }
    async fn direct_callback(
        &self,
        context: RpcContext<CallbackHandle>,
        message: String,
        count: usize,
    ) -> Result<(), DemoError> {
        context
            .handle()
            .acknowledge(format!("{message}:{count}"))
            .await
            .map_err(|_| DemoError)?;
        Ok(())
    }
    async fn numbers(&self, _: ()) -> Result<Stream<u64>, DemoError> {
        let dropped = StreamDrop(self.0.clone());
        Ok(Stream::new(async_stream::stream! {
            let _dropped = dropped;
            yield 1;
            futures_util::future::pending::<()>().await;
        }))
    }
}

#[tokio::test]
async fn calls_notifications_and_typed_callbacks_work_in_process() {
    let (left, right) = InProcessTransport::pair();
    let left = Peer::new(left);
    let right = Peer::new(right);
    let acknowledgements = Arc::new(AtomicUsize::new(0));
    left.register::<CallbackHandle, _>(CallbackService(acknowledgements.clone()));
    let events = Arc::new(AtomicUsize::new(0));
    right.register::<ServiceHandle, _>(EchoService(events.clone()));
    let service = ServiceHandle::new(left);
    assert_eq!(service.echo("hello".into()).await.unwrap(), "hello");
    assert_eq!(
        service.join("left".into(), "right".into()).await.unwrap(),
        "left:right"
    );
    service.event("event".into()).unwrap();
    service.direct_event("direct".into(), 2).unwrap();
    service.callback("ack".into()).await.unwrap();
    service.direct_callback("direct".into(), 2).await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(events.load(Ordering::SeqCst), 2);
    assert_eq!(acknowledgements.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_handle_can_delegate_to_an_in_process_implementation() {
    let events = Arc::new(AtomicUsize::new(0));
    let handle = EchoService(events.clone()).into_handle::<ServiceHandle>();
    assert_eq!(handle.echo("local".into()).await.unwrap(), "local");
}

#[tokio::test]
async fn direct_fields_honor_serde_attributes_on_the_wire() {
    let (caller_transport, receiver_transport) = duplex(1024);
    let caller = Peer::new(JsonTransport(Framed::new(
        caller_transport,
        LinesCodec::new(),
    )));
    let receiver = Peer::new(JsonTransport(Framed::new(
        receiver_transport,
        LinesCodec::new(),
    )));
    receiver.register::<ServiceHandle, _>(EchoService(Arc::new(AtomicUsize::new(0))));

    let response: String = caller
        .call(
            "join",
            serde_json::json!({"left": "left", "rightValue": "right"}),
        )
        .await
        .unwrap();
    assert_eq!(response, "left:right");

    let one_field: String = caller
        .call("echo", serde_json::json!({"request": "field"}))
        .await
        .unwrap();
    assert_eq!(one_field, "field");

    let payload: String = caller
        .call("raw_echo", serde_json::json!("payload"))
        .await
        .unwrap();
    assert_eq!(payload, "payload");
}

#[tokio::test]
async fn dropping_a_stream_sends_protocol_cancellation() {
    let events = Arc::new(AtomicUsize::new(0));
    let handle = EchoService(events.clone()).into_handle::<ServiceHandle>();
    let mut stream = handle.numbers(());
    assert_eq!(stream.next().await.unwrap().unwrap(), 1);
    drop(stream);
    tokio::task::yield_now().await;
    assert_eq!(events.load(Ordering::SeqCst), 1);
}
