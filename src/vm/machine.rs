use super::op::{OpWhich, OpAddr};
use std::cell::RefCell;
use capnp::message::Builder;
use smol::LocalExecutor;
use async_broadcast::broadcast;
// use super::op::{CodeReader, RegAddr, OpAddr, Op, OpPrimitive};
use crate::value::{
    ExtractValue,
    ValueBuilder,
    ValueWhich,
    ArgValueWhich
};

use crate::value::storage::{
    Storage, 
    ObjPointer, ObjectRef,
    DataRef,
};
use super::scope::{Registers, ExecQueue, Arg};
use super::ExecError;
use std::collections::HashMap;

pub type RegAddr = u16;


pub struct Machine<'s, S: Storage> {
    // the storage must be multi &-safe, but does not need to be threading safe
    store: &'s S, 
    // Since we use a local executor
    // we can safely use a refcell here
    // TODO: Switch from async_broadcast to a custom SPMC type. This is overkill
    exec: RefCell<HashMap<ObjPointer, async_broadcast::Receiver<()>>>
}

enum ForceRes<'s, S: Storage + 's> {
    Value(S::ValueRef<'s>), // the object whose value to copy into the original thunk
    TailCall(S::EntryRef<'s>) // the bound lambda to invoke
}

impl<'s, S: Storage> Machine<'s, S> {
    pub fn new(store: &'s S) -> Self {
        Self { 
            store,
            exec: RefCell::new(HashMap::new())
        }
    }

    async fn extract_base<'e>(&'e self, ex: &'e LocalExecutor<'e>,
                    entry_ptr: ObjPointer)
                            -> Result<(ObjPointer, Vec<ObjPointer>, Vec<Arg>), ExecError> {
        let mut code = self.store.get(entry_ptr)?;
        let mut closure : Vec<ObjPointer> = Vec::new();
        // the application spine, backwards (i.e top application is first)
        let mut apply_bkw = Vec::new();

        // keep extracting/forcing the LHS of applies
        // until we have actually hit the base code or closure
        loop {
            let code_data = code.get_value()?;
            use ValueWhich::*;
            match code_data.reader().which()? {
            Code(_) => break,
            Closure(c) => {
                for e in c.get_entries()?.iter() { closure.push(e.into()) }
                code = self.store.get(c.get_code().into())?;
                break
            },
            Apply(ap) => {
                for a in ap.get_args()?.iter().rev() {
                    use ArgValueWhich::*;
                    apply_bkw.push(match a.which()? {
                    Pos(_) => Arg::Pos(a.get_val().into()),
                    Key(k) => Arg::Key(k.into(), a.get_val().into()),
                    VarPos(_) => Arg::VarPos(a.get_val().into()),
                    VarKey(_) => Arg::VarKey(a.get_val().into()),
                    });
                }
                let thunk =  self.store.get(ap.get_lam().into())?;
                code = self.force(ex, thunk).await?;
            },
            _ => return Err(ExecError {})
            };
        }
        Ok((code.ptr(), closure, apply_bkw))
    }

    async fn force_task<'e>(&'e self, ex: &'e LocalExecutor<'e>, mut entry_ref: S::EntryRef<'s>) {
        let ret : Result<S::ValueRef<'s>, ExecError> = async {
            loop { // loop for tail call recursion
                let queue = ExecQueue::new();
                let regs = Registers::new(self.store);
                let (code_ptr, closure, args) =
                    self.extract_base(ex, entry_ref.ptr()).await?;
                // setup the pointers and get a reference to the code entry
                let code_data = self.store.get_value(code_ptr)?;
                let code_reader = code_data.reader().code().ok_or(ExecError {})?;
                regs.populate(&queue, code_reader, &closure, &args)?;

                // We need to drop the local executor before everything else, so define it last
                let queue_exec = LocalExecutor::new();
                let entry : ForceRes<'s, S> = queue_exec.run(async {
                    loop {
                        let addr : OpAddr = queue.next_op().await;
                        let op = code_reader.get_ops()?.get(addr as u32);
                        use OpWhich::*;
                        match op.which()? {
                        Ret(id) => {
                            return  Ok::<ForceRes<'s, S>, ExecError>(
                                ForceRes::Value(regs.consume(id)?.get_value()?)
                            );
                        },
                        TailRet(id) => {
                            // Tail-call into the entry
                            let entry = regs.consume(id)?;
                            return Ok::<ForceRes<'s, S>, ExecError>(ForceRes::TailCall(entry));
                        },
                        Force(r) => {
                            let entry = regs.consume(r.get_arg())?;
                            // spawn the force as a background task
                            // since we might want to move onto other things
                            let queue_ref = &queue;
                            let regs_ref = &regs;
                            queue_exec.spawn(async move {
                                let e= self.force(ex, entry).await.unwrap();
                                // we need to get 
                                regs_ref.set_object(r.get_dest().unwrap(), e).unwrap();
                                queue_ref.complete(r.get_dest().unwrap(), code_reader.reborrow()).unwrap();
                            }).detach();
                        },
                        RecForce(_) => panic!("Not implemented"),
                        Closure(r) => { // construct a closure
                            let code= regs.consume(r.get_code())?;
                            let entries : Result<Vec<S::EntryRef<'s>>, ExecError> = r.get_entries()?.into_iter()
                                        .map(|x| regs.consume(x)).collect();
                            let entries = entries?;
                            let result_data = {
                                let mut builder = Builder::new_default();
                                let mut root : ValueBuilder<'_> = builder.get_root()?;
                                let mut closure = root.reborrow().init_closure();
                                closure.set_code(code.ptr().unwrap());
                                let mut e = closure.init_entries(entries.len() as u32);
                                for (i, r) in entries.into_iter().enumerate() {
                                    e.set(i as u32, r.ptr().unwrap())
                                }
                                self.store.insert(root.into_reader())?
                            };
                            let entry = self.store.alloc()?;
                            entry.push_result(result_data);
                        },
                        _ => panic!("Not implemented")
                        }
                    }
                }).await?;
                match entry {
                ForceRes::Value(e) => return Ok::<S::ValueRef<'s>, ExecError>(e),
                ForceRes::TailCall(p) => entry_ref = p
                }
            }
        }.await;
        // Actually replace the thunk
        // TODO: Handle error and replace with error
        entry_ref.push_result(ret.unwrap())
    }

    // try_force will check if (a) the object being forced is in
    // fact a thunk and (b) if someone else is already forcing this thunk
    // matching with other force task
    pub async fn force<'e>(&'e self, ex: &'e LocalExecutor<'e>, thunk_ref: S::EntryRef<'s>) -> Result<S::EntryRef<'s>, ExecError> {
        // check if the it even is a pointer
        let thunk_lam_target = {
            let thunk_data = thunk_ref.get_value()?;
            match thunk_data.reader().thunk() {
                Some(s) => s,
                None => return Ok(thunk_ref)
            }
        };
        // check if the lambda is currently being forced
        // by looking into the force map
        let ptr = thunk_ref.ptr();
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
            e.insert(ptr, r.clone());
            // spawn a task to force
            let target = self.store.get(thunk_lam_target).unwrap();
            ex.spawn(async move { 
                self.force_task(ex, target).await;
                s.broadcast(()).await.unwrap();
            }).detach();
            Some(r)
        }).ok_or(ExecError{})?;
        // while we are waiting for the child scope, we
        // don't hold any references
        h.recv().await.unwrap();
        Ok(thunk_ref)
    }
}