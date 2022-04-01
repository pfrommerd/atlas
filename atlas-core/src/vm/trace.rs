// use futures_lite::future::BoxedLocal;
// use crate::store::Storage;
// use std::collections::HashMap;
// use super::resource::ResourceID;
// use std::rc::{Rc, Weak};

// use slab::Slab;
// use std::cell::RefCell;

// pub type LambdaHash = u64;
// pub type ShallowHash = u64;
// pub type ContextID = u64;

// pub type ObjectID = usize;
// pub type EventID = usize;

// pub enum Action<'s, S: Storage + 's> {
//     // Map a particular objectID to an input of the function
//     SetInput(ObjectID, usize),
//     Query(ObjectID),
//     Result(ObjectID, ShallowHash),
//     UseResource(ResourceID),
//     Print(String),
//     Ret(S::Handle<'s>)
// }

// pub struct TraceContext<'t, 's, S: Storage> {
//     // The parent tracing context, for use
//     // when one tracing function calls another
//     // tracing function internally
//     parent: Option<&'t mut TraceContext<'t, 's, S>>,
//     // Trace object to output to
//     target: Option<&'t mut Trace<'s, S>>,
//     // Current trace set which we are in
//     trace_set: Option<&'t TraceSet<'s, S>>,
//     object_map: HashMap<ObjectID, S::Handle<'s>>
// }

// pub struct TraceEvent<'s, S: Storage + 's> {
//     action: Action<'s, S>,
//     deps: Vec<EventID>,
//     children: Vec<EventID>
// }

// pub struct Trace<'s, S: Storage + 's> {
//     events: Slab<TraceEvent<'s, S>>,
// }

// pub struct TraceSet<'s, S: Storage + 's> {
//     root_trace: Trace<'s, S>,
//     // sub-traces for any thunks returned by the root trace
// }

// pub struct CacheEntry<'s, S: Storage + 's> {
//     root_traces: TraceSet<'s, S>,
//     thunk_traces: HashMap<S::Handle<'s>, TraceSet<'s, S>>
// }

// pub struct Cache<'s, S: Storage + 's> {
//     traces: HashMap<LambdaHash, TraceSet<'s, S>>,
//     contexts: HashMap<ContextID, Context<'s, S>>
// }

// use crate::store::Storage;
// use super::machine::Machine;
// use crate::Error;
// use std::marker::PhantomData;
// use std::collections::HashMap;
// use std::cell::RefCell;

// pub trait TraceBuilder<'s, 'c, S: Storage> {
//     // will consume the builder and finish the trace
//     fn returned(self, value: S::Handle<'s>);
// }

// pub enum Lookup<'s, 'c, S, T>
//         where
//             S : Storage + 's,
//             T : TraceBuilder<'s, 'c, S> {
//     Hit(S::Handle<'s>), // yay we found a result in the cache
//     // A miss. Atomically registers and returns a new trace
//     // for the thunk that was requested
//     Miss(T, PhantomData<&'c T>)
// }

// type CacheFuture<'a, T> = std::pin::Pin<Box<dyn futures_lite::Future<Output=T> + 'a>>;

// pub trait Cache<'s, S : Storage + 's> : Sized {
//     type TraceBuilder<'c> : TraceBuilder<'s, 'c, S> where Self : 'c, 's : 'c, S: 's, S: 'c;

//     fn query<'c>(&'c self, mach: &'c Machine<'s, Self, S>, 
//                     thunk_ref: &'c S::Handle<'s>)
//             -> CacheFuture<'c, Result<Lookup<'s, 'c, S, Self::TraceBuilder<'c>>, Error>>;
// }


// enum ThunkStatus<'s, S: Storage + 's> {
//     InProgres(async_broadcast::Receiver<S::Handle<'s>>),
//     Finished(S::Handle<'s>)
// }

// // An execution cache which just keeps track of
// // if a particular thunk is being forced
// pub struct ThunkCache<'s, S: Storage + 's> {
//     map: RefCell<HashMap<S::Handle<'s>, ThunkStatus<'s, S>>>
// }

// impl<'s, S: Storage> ThunkCache<'s, S> {
//     pub fn new() -> Self {
//         Self { map : RefCell::new(HashMap::new()) }
//     }
// }

// pub struct DirectForceBuilder<'s, 'c, S: Storage> {
//     ptr: S::Handle<'s>,
//     cache: &'c ThunkCache<'s, S>,
//     sender: async_broadcast::Sender<S::Handle<'s>>
// }

// impl<'s, 'c, S: Storage> TraceBuilder<'s, 'c, S> for DirectForceBuilder<'s, 'c, S> {
//     fn returned(self, value: S::Handle<'s>) {
//         let mut map = self.cache.map.borrow_mut();
//         let old = map.insert(self.ptr, ThunkStatus::Finished(value.clone()));
//         self.sender.try_broadcast(value).unwrap();
//         std::mem::drop(old); // keep the old receiver around until after we have broadcasted to prevent closing the channel
//     }
// }

// impl<'s, S: Storage> Cache<'s, S> for ThunkCache<'s, S> {
//     type TraceBuilder<'c> = DirectForceBuilder<'s, 'c, S> where Self : 'c;

//     fn query<'c>(&'c self, _mach: &'c Machine<'s, Self, S>, thunk_ref: &'c S::Handle<'s>)
//             -> CacheFuture<'c, Result<Lookup<'s, 'c, S, Self::TraceBuilder<'c>>, Error>> {
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