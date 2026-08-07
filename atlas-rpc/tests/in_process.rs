use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use atlas_rpc::{interface, InProcessTransport, IntoHandle, Peer, RpcContext, Stream};
use futures_util::StreamExt;
use serde::Serialize;

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

    #[rpc(notification)]
    async fn event(&self, request: String) -> Result<(), DemoError>;

    async fn callback(
        &self,
        #[rpc(context)] context: RpcContext<CallbackHandle>,
        request: String,
    ) -> Result<(), DemoError>;

    #[rpc(stream)]
    async fn numbers(&self, request: ()) -> Result<Stream<u64>, DemoError>;
}

struct CallbackService(Arc<AtomicUsize>);
impl Callback for CallbackService {
    async fn acknowledge(&self, request: String) -> Result<(), DemoError> {
        assert_eq!(request, "ack");
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
    async fn event(&self, request: String) -> Result<(), DemoError> {
        assert_eq!(request, "event");
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
    service.event("event".into()).unwrap();
    service.callback("ack".into()).await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(events.load(Ordering::SeqCst), 1);
    assert_eq!(acknowledgements.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_handle_can_delegate_to_an_in_process_implementation() {
    let events = Arc::new(AtomicUsize::new(0));
    let handle = EchoService(events.clone()).into_handle::<ServiceHandle>();
    assert_eq!(handle.echo("local".into()).await.unwrap(), "local");
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
