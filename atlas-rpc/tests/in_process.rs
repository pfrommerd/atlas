use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use atlas_rpc::{interface, InProcessTransport, Peer, RpcContext};
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
        #[rpc(context)] context: RpcContext<CallbackClient>,
        request: String,
    ) -> Result<(), DemoError>;
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
        context: RpcContext<CallbackClient>,
        request: String,
    ) -> Result<(), DemoError> {
        context
            .client()
            .acknowledge(request)
            .await
            .map_err(|_| DemoError)?;
        Ok(())
    }
}

#[tokio::test]
async fn calls_notifications_and_typed_callbacks_work_in_process() {
    let (left, right) = InProcessTransport::pair();
    let left = Peer::new(left);
    let right = Peer::new(right);
    let acknowledgements = Arc::new(AtomicUsize::new(0));
    CallbackServer::new(CallbackService(acknowledgements.clone())).register(&left);
    let events = Arc::new(AtomicUsize::new(0));
    ServiceServer::new(EchoService(events.clone())).register(&right);
    let service = ServiceClient::new(left);
    assert_eq!(service.echo("hello".into()).await.unwrap(), "hello");
    service.event("event".into()).unwrap();
    service.callback("ack".into()).await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(events.load(Ordering::SeqCst), 1);
    assert_eq!(acknowledgements.load(Ordering::SeqCst), 1);
}
