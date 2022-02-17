use std::marker::PhantomData;

use crate::value::{Allocator, ObjHandle};
use super::machine::Machine;
use super::ExecError;

use std::collections::HashMap;
use std::cell::RefCell;

pub trait TraceBuilder<'a, 'c, A: Allocator> {
    // will consume the builder and finish the trace
    fn returned(self, value: ObjHandle<'a, A>);
}

pub enum Lookup<'a, 'c, A, T>
        where
            A : Allocator,
            T : TraceBuilder<'a, 'c, A> {
    Hit(ObjHandle<'a, A>), // yay we found a result in the cache
    // A miss. Atomically registers and returns a new trace
    // for the thunk that was requested
    Miss(T, PhantomData<&'c T>)
}
// pub struct ExecCache<'s, S: Storage> {
//     phantom : PhantomData<&'s S>,
//     thunk_exec: RefCell<HashMap<ObjPointer, async_broadcast::Receiver<()>>>,
// }


type CacheFuture<'a, T> = std::pin::Pin<Box<dyn futures_lite::Future<Output=T> + 'a>>;

pub trait ExecCache<'a, A : Allocator> {
    type TraceBuilder<'c> : TraceBuilder<'a, 'c, A> where Self : 'c, 'a : 'c, A: 'a, A: 'c;

    fn query<'c>(&'c self, mach: &'c Machine<'a, '_, A, Self>, thunk_ref: &'c ObjHandle<'a, A>)
            -> CacheFuture<'c, Result<Lookup<'a, 'c, A, Self::TraceBuilder<'c>>, ExecError>>;
}


enum ThunkStatus<'a, A: Allocator> {
    InProgres(async_broadcast::Receiver<ObjHandle<'a, A>>),
    Finished(ObjHandle<'a, A>)
}

// An execution cache which just keeps track of
// if a particular thunk is being forced
pub struct ForceCache<'a, A: Allocator> {
    map: RefCell<HashMap<ObjHandle<'a, A>, ThunkStatus<'a, A>>>
}

impl<'a, A: Allocator> ForceCache<'a, A> {
    pub fn new() -> Self {
        Self { map : RefCell::new(HashMap::new()) }
    }
}

pub struct DirectForceBuilder<'a, 'c, A: Allocator> {
    ptr: ObjHandle<'a, A>,
    cache: &'c ForceCache<'a, A>,
    sender: async_broadcast::Sender<ObjHandle<'a, A>>
}

impl<'a, 'c, A: Allocator> TraceBuilder<'a, 'c, A> for DirectForceBuilder<'a, 'c, A> {
    fn returned(self, value: ObjHandle<'a, A>) {
        let mut map = self.cache.map.borrow_mut();
        let old = map.insert(self.ptr, ThunkStatus::Finished(value.clone()));
        self.sender.try_broadcast(value).unwrap();
        std::mem::drop(old); // keep the old receiver around until after we have broadcasted to prevent closing the channel
    }
}

impl<'a, A: Allocator> ExecCache<'a, A> for ForceCache<'a, A> {
    type TraceBuilder<'c> where Self: 'c = DirectForceBuilder<'a, 'c, A>;

    fn query<'c>(&'c self, _mach: &'c Machine<'a, '_, A, Self>, thunk_ref: &'c ObjHandle<'a, A>)
            -> CacheFuture<'c, Result<Lookup<'a, 'c, A, Self::TraceBuilder<'c>>, ExecError>> {
        Box::pin(async move {
            let mut map = self.map.borrow_mut();
            let s = map.get(&thunk_ref);
            match s {
                None => {
                    // insert an in-progress status
                    let (s, r) = async_broadcast::broadcast(1);
                    map.insert(thunk_ref.clone(), ThunkStatus::InProgres(r));
                    Ok(Lookup::Miss(DirectForceBuilder { ptr: thunk_ref.clone(), cache: self, sender: s }, PhantomData))
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