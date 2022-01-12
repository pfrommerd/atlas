use super::op::{OpReader, OpWhich};
use std::cell::RefCell;
use smol::LocalExecutor;
use async_broadcast::broadcast;
// use super::op::{CodeReader, RegAddr, OpAddr, Op, OpPrimitive};
use crate::value::{Storage, Pointer, ValueWhich, storage::ValueEntry};
use super::scope::Scope;
use std::collections::HashMap;

pub type RegAddr = u16;

pub struct ExecError {}

impl From<capnp::Error> for ExecError {
    fn from(_: capnp::Error) -> Self {
        Self {}
    }
}
impl From<capnp::NotInSchema> for ExecError {
    fn from(_: capnp::NotInSchema) -> Self {
        Self {}
    }
}


pub struct Machine<'s, S: Storage> {
    // the storage must be multi &-safe, but does not need to be threading safe
    store: &'s S, 
    // Since we use a local executor
    // we can safely use a refcell here
    // TODO: Switch from async_broadcast to a custom SPMC type. This is overkill
    exec: RefCell<HashMap<Pointer, async_broadcast::Receiver<()>>>
}

impl<'s, S: Storage> Machine<'s, S> {
    pub fn new(store: &'s S) -> Self {
        Self { store: store, exec: RefCell::new(HashMap::new()) }
    }

    async fn force_task<'e>(&'e self, ex: &'e LocalExecutor<'e>, thunk: Pointer)
                            -> Result<S::Entry<'s>, ExecError> {
        // Create a scope from the thunk
        let mut scope = Scope::from_thunk(thunk, self.store)?;

        // we keep the code as a separate 
        // so that we don't borrow scope immutably
        // and so can mutate it
        let code_entry = self.store.get(scope.code().ptr())
                .ok_or(ExecError{})?;
        let r = code_entry.reader();
        let cr = match r.which()? {
            ValueWhich::Code(c) => c?,
            _ => return Err(ExecError{})
        };
        loop {
            let op = scope.current(&cr)?;
            match op.which()? {
                OpWhich::Ret(r) => {
                    let e = scope.reg(r).ok_or(ExecError{})?;
                    // we are still covered by e when we re-get
                    // the return value so this is fine
                    return self.store.get(e.ptr()).ok_or(ExecError{});
                }
                _ => self.run_op(ex, &mut scope, op).await?,
            }
        }
    }

    pub async fn force<'e>(&'e self, ex: &'e LocalExecutor<'e>, val: &S::Entry<'_>) 
                                -> Result<(), ExecError> {
        // check if the value is a thunk, otherwise just continue
        // borrow a reference to the exec map
        // since we never borrow over an await bound, this is safe

        // It is not a thunk!
        let ptr = val.ptr();
        let mut h = {
            let e = self.exec.borrow();
            match e.get(&ptr) {
                Some(h) => Some(h.clone()),
                None => None
            }
        }.or_else(|| {
            // insert a new thunk execution receiver into the map and return that instead
            let mut e = self.exec.borrow_mut();
            let (s, r) = broadcast::<()>(1);
            e.insert(val.ptr(), r.clone());
            // spawn a task to force
            ex.spawn(async move { 
                let res = self.force_task(ex, ptr).await;
                // Redirect ptr to the associated force task
                match res {
                    Ok(res_ptr) => {
                        self.store.set(ptr, res_ptr.ptr());
                    },
                    Err(_) => panic!("Canot handle error")
                }
                s.broadcast(()).await.unwrap();
            }).detach();
            Some(r)
        }).ok_or(ExecError{})?;
        // while we are waiting for the child scope, we
        // don't hold any references
        h.recv().await.unwrap();
        Ok(())
    }

    async fn run_op<'e>(&'e self, ex: &'e LocalExecutor<'e>,
                        scope: &mut Scope<'_, S>, op: OpReader<'_>) 
                                    -> Result<(), ExecError> {
        use OpWhich::*;
        match op.which()? {
            Force(r) => {
                let entry = scope.reg(r).ok_or(ExecError {})?;
                self.force(ex, entry).await
            },
            Ret(_) => panic!("Should not reach return!"), // This should be handled in the thunk execution
            _ => panic!("Unimplemented op")
        }
    }

}

// impl<'a> Machine<'a> {
//     pub fn new(arena: &'a Arena,
//                entrypoint: Pointer) -> Self {
//         Machine {
//             arena,
//             stack: Vec::new(),
//             current: Scope::new(entrypoint)
//         }
//     }

//     fn store(scope: &mut Scope, reg: RegAddr, val: OpPrimitive) {

//     }

//     fn run_op(scope: &mut Scope, op: Op) -> OpRes {
//         use Op::*;
//         match op {
//             Store(reg, prim) => panic!("not implemented"),
//             _ => panic!("Unimplemented op type"),
//         }
//     }

//     pub fn run(&mut self) {

//     }
// }