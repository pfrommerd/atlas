use crate::store::Storage;
use std::collections::HashMap;
use super::resource::ResourceID;
use std::rc::{Rc, Weak};

use slab::Slab;
use std::cell::RefCell;

pub type LambdaHash = u64;
pub type ContextID = u64;

pub type ObjectID = usize;
pub type EventID = usize;

pub enum Action<'s, S: Storage + 's> {
    SetInput(ObjectID, usize),
    Query(ObjectID),
    UseResource(ResourceID),
    Ret(S::Handle<'s>)
}

pub struct TraceEvent<'s, S: Storage + 's> {
    action: Action<'s, S>,
    parents: Vec<EventID>,
    children: Vec<EventID>
}

pub struct Trace<'s, S: Storage + 's> {
    // Starting events with no dependencies
    roots: Vec<EventID>,
    events: Slab<TraceEvent<'s, S>>,
    next_event_id: EventID
}

pub struct TraceSet<'s, S: Storage + 's> {
    lambda_trace: Trace<'s, S>,
    // sub-traces for any thunks returned by the root trace
    thunk_traces: HashMap<S::Handle<'s>, Trace<'s, S>>,
    next_object_id: ObjectID
}

pub struct Context<'s, S: Storage + 's> {
    inputs: Vec<S::Handle<'s>>,
    objects: HashMap<ObjectID, S::Handle<'s>>
}

pub struct Cache<'s, S: Storage + 's> {
    traces: HashMap<LambdaHash, TraceSet<'s, S>>,
    contexts: HashMap<ContextID, Context<'s, S>>
}

// pub trait TraceBuilder<'a, 'c, S: Storage> {
//     // will consume the builder and finish the trace
//     fn returned(self, value: ObjHandle<'a, A>);
// }

// pub enum Lookup<'a, 'c, A, T>
//         where
//             A : Storage,
//             T : TraceBuilder<'a, 'c, A> {
//     Hit(ObjHandle<'a, A>), // yay we found a result in the cache
//     // A miss. Atomically registers and returns a new trace
//     // for the thunk that was requested
//     Miss(T, PhantomData<&'c T>)
// }
// // pub struct ExecCache<'s, S: Storage> {
// //     phantom : PhantomData<&'s S>,
// //     thunk_exec: RefCell<HashMap<ObjPointer, async_broadcast::Receiver<()>>>,
// // }


// type CacheFuture<'a, T> = std::pin::Pin<Box<dyn futures_lite::Future<Output=T> + 'a>>;

// pub trait ExecCache<'a, A : Storage> {
//     type TraceBuilder<'c> : TraceBuilder<'a, 'c, A> where Self : 'c, 'a : 'c, A: 'a, A: 'c;

//     fn query<'c>(&'c self, mach: &'c Machine<'a, '_, A, Self>, thunk_ref: &'c ObjHandle<'a, A>)
//             -> CacheFuture<'c, Result<Lookup<'a, 'c, A, Self::TraceBuilder<'c>>, Error>>;
// }


// enum ThunkStatus<'a, S: Storage> {
//     InProgres(async_broadcast::Receiver<ObjHandle<'a, A>>),
//     Finished(ObjHandle<'a, A>)
// }

// // An execution cache which just keeps track of
// // if a particular thunk is being forced
// pub struct ForceCache<'a, S: Storage> {
//     map: RefCell<HashMap<ObjHandle<'a, A>, ThunkStatus<'a, A>>>
// }

// impl<'a, S: Storage> ForceCache<'a, A> {
//     pub fn new() -> Self {
//         Self { map : RefCell::new(HashMap::new()) }
//     }
// }

// pub struct DirectForceBuilder<'a, 'c, S: Storage> {
//     ptr: ObjHandle<'a, A>,
//     cache: &'c ForceCache<'a, A>,
//     sender: async_broadcast::Sender<ObjHandle<'a, A>>
// }

// impl<'a, 'c, S: Storage> TraceBuilder<'a, 'c, A> for DirectForceBuilder<'a, 'c, A> {
//     fn returned(self, value: ObjHandle<'a, A>) {
//         let mut map = self.cache.map.borrow_mut();
//         let old = map.insert(self.ptr, ThunkStatus::Finished(value.clone()));
//         self.sender.try_broadcast(value).unwrap();
//         std::mem::drop(old); // keep the old receiver around until after we have broadcasted to prevent closing the channel
//     }
// }

// impl<'a, S: Storage> ExecCache<'a, A> for ForceCache<'a, A> {
//     type TraceBuilder<'c> where Self: 'c = DirectForceBuilder<'a, 'c, A>;

//     fn query<'c>(&'c self, _mach: &'c Machine<'a, '_, A, Self>, thunk_ref: &'c ObjHandle<'a, A>)
//             -> CacheFuture<'c, Result<Lookup<'a, 'c, A, Self::TraceBuilder<'c>>, Error>> {
//         Box::pin(async move {
//             let mut map = self.map.borrow_mut();
//             let s = map.get(&thunk_ref);
//             match s {
//                 None => {
//                     // insert an in-progress status
//                     let (s, r) = async_broadcast::broadcast(1);
//                     map.insert(thunk_ref.clone(), ThunkStatus::InProgres(r));
//                     Ok(Lookup::Miss(DirectForceBuilder { ptr: thunk_ref.clone(), cache: self, sender: s }, PhantomData))
//                 }
//                 Some(status) => {
//                     match status {
//                         ThunkStatus::Finished(v) => Ok(Lookup::Hit(v.clone())),
//                         ThunkStatus::InProgres(r) => {
//                             let mut r = r.clone();
//                             std::mem::drop(map); // we don't want to hold on the map refmut over the await
//                             let v = r.recv().await.unwrap();
//                             Ok(Lookup::Hit(v))
//                         }
//                     }
//                 }
//             }
//         })
//     }
// }