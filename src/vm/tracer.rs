use std::marker::PhantomData;

use crate::value::Storage;
use super::machine::Machine;
use super::ExecError;

use futures_lite::future::BoxedLocal;

pub trait TraceRef<'s, 'e, S: Storage> {

}

pub enum Lookup<'s, 'c, S, T>
        where
            S : Storage + 's, 
            T : TraceRef<'s, 'c, S> {
    Hit(S::ValueRef<'s>), // yay we found a result in the cache
    // We get this if the exact thunk we asked for was
    // in the process of being 
    // executed and now the result is ready
    // (i.e it is a hit where we don't even have to do anything)
    Ready, 
    // A miss. Atomically registers and returns a new trace
    // for the thunk that was requested
    Miss(T, PhantomData<&'c T>)
}
// pub struct ExecCache<'s, S: Storage> {
//     phantom : PhantomData<&'s S>,
//     thunk_exec: RefCell<HashMap<ObjPointer, async_broadcast::Receiver<()>>>,
// }

pub trait ExecCache<'s, S : Storage> {
    type TraceRef<'c> : TraceRef<'s, 'c, S> where Self : 'c, S : 's;

    fn query<'c>(&'c self, mach: &Machine<'s, 'c, S, Self>, thunk_ref: &S::EntryRef<'s>)
            -> BoxedLocal<Result<Lookup<'s, 'c, S, Self::TraceRef<'c>>, ExecError>>;
}


// An execution cache which just keeps track of
// if a particular thunk is being forced
pub struct ForceCache {
}

impl ForceCache {
    pub fn new() -> Self {
        Self {}
    }
}

pub struct ForceRef<'e> {
    c: &'e ForceCache
}

impl<'s, 'e, S: Storage + 's> TraceRef<'s, 'e, S> for ForceRef<'e> {

}

impl<'s, S: Storage> ExecCache<'s, S> for ForceCache {
    type TraceRef<'e> where Self: 'e, S: 's = ForceRef<'e>;

    fn query<'e>(&'e self, mach: &Machine<'s, 'e, S, Self>, thunk_ref: &S::EntryRef<'s>)
            -> BoxedLocal<Result<Lookup<'s, 'e, S, Self::TraceRef<'e>>, ExecError>> {
        panic!()
    }
}