use super::op::{OpWhich, OpAddr};
use std::cell::RefCell;
use smol::LocalExecutor;
use async_broadcast::broadcast;
// use super::op::{CodeReader, RegAddr, OpAddr, Op, OpPrimitive};
use crate::value::{
    ExtractValue,
    ValueWhich,
    ArgValueWhich
};

use crate::value::storage::{
    ObjectStorage, 
    ObjPointer, ObjectRef,
    DataStorage, DataRef,
};
use super::scope::{Registers, ExecQueue, Arg};
use super::ExecError;
use std::collections::HashMap;

pub type RegAddr = u16;


pub struct Machine<'s, S: ObjectStorage, D: DataStorage> {
    // the storage must be multi &-safe, but does not need to be threading safe
    store: &'s S, 
    data: &'s D,
    // Since we use a local executor
    // we can safely use a refcell here
    // TODO: Switch from async_broadcast to a custom SPMC type. This is overkill
    exec: RefCell<HashMap<ObjPointer, async_broadcast::Receiver<()>>>
}

enum ForceRes<'s, S: ObjectStorage + 's, D: DataStorage + 's> {
    Value(D::EntryRef<'s>),
    TailCall(S::EntryRef<'s>) // the bound lambda to invoke
}

impl<'s, S: ObjectStorage, D: DataStorage> Machine<'s, S, D> {
    pub fn new(store: &'s S, data: &'s D) -> Self {
        Self { store, data, exec: RefCell::new(HashMap::new()) }
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
            let code_data = code.get_data(self.data)?;
            use ValueWhich::*;
            match code_data.value().which()? {
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
        let ret : Result<D::EntryRef<'s>, ExecError> = async {
            loop { // loop for tail call recursion
                let (code_ptr, closure, args) =
                    self.extract_base(ex, entry_ref.ptr()).await?;
                let code_data = self.store.get_data(code_ptr, self.data)?;
                let code_reader = code_data.value().code().ok_or(ExecError{})?;

                let queue = ExecQueue::new();
                let regs = Registers::new(self.store);
                regs.populate(&queue, code_reader.reborrow(), &closure, &args)?;

                // We need to drop the local executor before everything else, so define it last
                let queue_exec = LocalExecutor::new();
                let entry = queue_exec.run(async {
                    loop {
                        let addr : OpAddr = queue.next_op().await;
                        let op = code_reader.get_ops()?.get(addr as u32);
                        use OpWhich::*;
                        match op.which()? {
                        Ret(id) => {
                            return  Ok::<ForceRes<'s, S, D>, ExecError>(ForceRes::Value::<'s, S, D>(regs.consume(id)?.get_data(self.data)?));
                        },
                        TailRet(id) => {
                            // Tail-call into the entry
                            let entry = regs.consume(id)?;
                            return Ok::<ForceRes<'s, S, D>, ExecError>(ForceRes::TailCall(entry));
                        },
                        Force(r) => {
                            let entry = regs.consume(r.get_arg())?;
                            // spawn the force as a background task
                            // since we might want to move onto other things
                            let queue_ref = &queue;
                            let regs_ref = &regs;
                            queue_exec.spawn(async move {
                                let e= self.force(ex, entry).await.unwrap();
                                regs_ref.set_object(r.get_dest().unwrap(), e).unwrap();
                                queue_ref.complete(r.get_dest().unwrap(), code_reader.reborrow()).unwrap();
                            }).detach();
                        },
                        _ => panic!("Not implemented")
                        }
                    }
                }).await;
                match entry? {
                ForceRes::Value(d) => return Ok::<D::EntryRef<'s>, ExecError>(d),
                ForceRes::TailCall(p) => entry_ref = p
                }
            }
        }.await;
        // Actually replace the thunk
        // TODO: Handle error and replace with error
        entry_ref.push_result(ret.unwrap().ptr())
    }

    // try_force will check if (a) the object being forced is in
    // fact a thunk and (b) if someone else is already forcing this thunk
    // matching with other force task
    pub async fn force<'e>(&'e self, ex: &'e LocalExecutor<'e>, thunk_ref: S::EntryRef<'s>) -> Result<S::EntryRef<'s>, ExecError> {
        // check if the it even is a pointer
        let thunk_data= thunk_ref.get_data(self.data)?;
        let thunk_lam_target = match thunk_data.value().thunk() {
            Some(s) => s,
            None => return Ok(thunk_ref)
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