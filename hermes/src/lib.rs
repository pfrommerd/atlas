use std::{ops::Deref, sync::Arc};

pub use async_trait::async_trait;
pub use std::io::{Error, ErrorKind, Result};
pub use hermes_derive::*;

pub mod ipc;

// Request-reply related transport

pub trait Request {
    type Reply;
}

#[async_trait(?Send)]
pub trait Service<R: Request> {
    async fn dispatch(&self, req: R) 
            -> Result<<R as Request>::Reply>;
}

#[async_trait(?Send)]
pub trait DispatchInto<S> : Request {
    async fn dispatch_into(self, s : &S) -> Result<Self::Reply>;
}

#[async_trait(?Send)]
impl<R, S> Service<R> for S where R : DispatchInto<S> + 'static {
    async fn dispatch(&self, req: R) 
            -> Result<<R as Request>::Reply> {
        req.dispatch_into(self).await
    }
}

pub struct Handle<R: Request> {
    handle: Arc<dyn Service<R>>
}

impl<R: Request> Handle<R> {
    pub fn new<S: Service<R> + 'static>(s: S) -> Self {
        Handle { handle: Arc::new(s) }
    }
}

#[async_trait(?Send)]
impl<R> DispatchInto<Handle<R>> for R where R: Request {
    async fn dispatch_into(self, s: &Handle<R>) 
            -> Result<<R as Request>::Reply> {
        s.handle.deref().dispatch(self).await
    }
}