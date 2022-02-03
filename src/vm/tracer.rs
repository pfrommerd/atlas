use std::marker::PhantomData;

use crate::value::{Storage, ObjectRef, ObjPointer};
use super::machine::Machine;
use super::ExecError;

use std::collections::HashMap;
use std::cell::RefCell;

pub trait TraceBuilder<'s, 'c, S: Storage + 's> {
    // will consume the builder and finish the trace
    fn returned(self, value: S::ObjectRef<'s>);
}

pub enum Lookup<'s, 'c, S, T>
        where
            S : Storage + 's, 
            T : TraceBuilder<'s, 'c, S> {
    Hit(S::ObjectRef<'s>), // yay we found a result in the cache
    // A miss. Atomically registers and returns a new trace
    // for the thunk that was requested
    Miss(T, PhantomData<&'c T>)
}
// pub struct ExecCache<'s, S: Storage> {
//     phantom : PhantomData<&'s S>,
//     thunk_exec: RefCell<HashMap<ObjPointer, async_broadcast::Receiver<()>>>,
// }


type CacheFuture<'a, T> = std::pin::Pin<Box<dyn futures_lite::Future<Output=T> + 'a>>;

pub trait ExecCache<'s, S : Storage> {
    type TraceBuilder<'c> : TraceBuilder<'s, 'c, S> where Self : 'c, 's: 'c, S : 's;

    fn query<'c>(&'c self, mach: &'c Machine<'s, '_, S, Self>, thunk_ref: &'c S::ObjectRef<'s>)
            -> CacheFuture<'c, Result<Lookup<'s, 'c, S, Self::TraceBuilder<'c>>, ExecError>>;
}


enum ThunkStatus<'s, S: Storage + 's> {
    InProgres(async_broadcast::Receiver<S::ObjectRef<'s>>),
    Finished(S::ObjectRef<'s>)
}

// An execution cache which just keeps track of
// if a particular thunk is being forced
pub struct ForceCache<'s, S: Storage + 's> {
    map: RefCell<HashMap<ObjPointer, ThunkStatus<'s, S>>>
}

impl<'s, S: Storage + 's> ForceCache<'s, S> {
    pub fn new() -> Self {
        Self { map : RefCell::new(HashMap::new()) }
    }
}

pub struct DirectForceBuilder<'s, 'c, S: Storage + 's> {
    ptr: ObjPointer,
    cache: &'c ForceCache<'s, S>,
    sender: async_broadcast::Sender<S::ObjectRef<'s>>
}

impl<'s, 'c, S: Storage + 's> TraceBuilder<'s, 'c, S> for DirectForceBuilder<'s, 'c, S> {
    fn returned(self, value: S::ObjectRef<'s>) {
        let mut map = self.cache.map.borrow_mut();
        let old = map.insert(self.ptr, ThunkStatus::Finished(value.clone()));
        self.sender.try_broadcast(value).unwrap();
        std::mem::drop(old); // keep the old receiver around until after we have broadcasted to prevent closing the channel
    }
}

impl<'s, S: Storage + 's> ExecCache<'s, S> for ForceCache<'s, S> {
    type TraceBuilder<'c> where Self: 'c, 's: 'c = DirectForceBuilder<'s, 'c, S>;

    fn query<'c>(&'c self, _mach: &'c Machine<'s, '_, S, Self>, thunk_ref: &'c S::ObjectRef<'s>)
            -> CacheFuture<'c, Result<Lookup<'s, 'c, S, Self::TraceBuilder<'c>>, ExecError>> {
        Box::pin(async {
            let ptr = thunk_ref.ptr();
            let mut map = self.map.borrow_mut();
            let s = map.get(&ptr);
            match s {
                None => {
                    // insert an in-progress status
                    let (s, r) = async_broadcast::broadcast(1);
                    map.insert(ptr, ThunkStatus::InProgres(r));
                    Ok(Lookup::Miss(DirectForceBuilder { ptr, cache: self, sender: s }, PhantomData))
                }
                Some(status) => {
                    match status {
                        ThunkStatus::Finished(v) => Ok(Lookup::Hit(v.clone())),
                        ThunkStatus::InProgres(r) => {
                            let mut r = r.clone();
                            std::mem::drop(map); // we don't want to hold on the map refmut over the await
                            let v = r.recv().await.unwrap();
                            Ok(Lookup::Hit(v))
                        }
                    }
                }
            }
        })
    }
}