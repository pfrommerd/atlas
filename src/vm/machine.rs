use super::op::{OpWhich, OpReader, OpAddr, CodeReader, MatchReader};
use std::cell::RefCell;
use super::builtin;
use futures_lite::FutureExt;
use smol::LocalExecutor;
use async_broadcast::broadcast;
// use super::op::{CodeReader, RegAddr, OpAddr, Op, OpPrimitive};
use crate::value::{
    ExtractValue,
    ValueWhich,
    ValueReader
};

use crate::value::storage::{
    Storage, StorageError,
    ObjPointer, ObjectRef,
    DataRef,
};
use super::scope::{Registers, ExecQueue};
use super::ExecError;
use std::collections::HashMap;

pub type RegAddr = u16;


pub struct Machine<'s, S: Storage> {
    // the storage must be multi &-safe, but does not need to be threading safe
    pub store: &'s S, 
    // Since we use a local executor
    // we can safely use a refcell here
    // TODO: Switch from async_broadcast to a custom SPMC type. This is overkill
    thunk_exec: RefCell<HashMap<ObjPointer, async_broadcast::Receiver<()>>>
}

enum OpRes<'s, S: Storage + 's> {
    Continue,
    Ret(S::ValueRef<'s>), // the object whose value to copy into the original thunk
    TailCall(S::EntryRef<'s>) // the bound lambda to invoke
}

impl<'s, S: Storage> Machine<'s, S> {
    pub fn new(store: &'s S) -> Self {
        Self { 
            store,
            thunk_exec: RefCell::new(HashMap::new())
        }
    }

    async fn force_task<'e>(&'e self, thunk_ref: S::EntryRef<'s>) -> Result<S::ValueRef<'s>, ExecError> {
        let mut entry_ref = self.store.get_value(
            thunk_ref.get_value()?.reader().thunk().ok_or(ExecError {})?
        )?;
        loop { // loop for tail call recursion
            let queue = ExecQueue::new();
            let regs = Registers::new(self.store);
            let (code_value, args) = match entry_ref.reader().which()? {
                ValueWhich::Code(_) => (entry_ref.clone(), Vec::new()),
                ValueWhich::Partial(r) => {
                    let r = r?;
                    let code_ref = self.store.get_value(ObjPointer::from(r.get_code()))?;
                    let args : Result<Vec<S::EntryRef<'s>>, StorageError> = r.get_args()?.into_iter()
                                .map(|x| self.store.get(ObjPointer::from(x))).collect();
                    (code_ref, args?)
                },
                _ => return Err(ExecError {})
            };
            let code_reader = code_value.reader().code().ok_or(ExecError {})?;
            regs.populate(&queue, code_reader, args)?;

            // We need to drop the local executor before the queue, regs
            let thunk_ex = LocalExecutor::new();
            let entry : OpRes<'s, S> = thunk_ex.run(async {
                loop {
                    let addr : OpAddr = queue.next_op().await;
                    let op = code_reader.get_ops()?.get(addr as u32);
                    let res = self.exec_op(op, code_reader.reborrow(), &thunk_ex, &regs, &queue).await;
                    match res? {
                        OpRes::Continue => {},
                        OpRes::Ret(r)  => {
                            return Ok::<OpRes<'s, S>, ExecError>(OpRes::Ret(r))
                        }
                        OpRes::TailCall(r) => {
                            return Ok::<OpRes<'s, S>, ExecError>(OpRes::TailCall(r))
                        }
                    }
                }
            }).await?;
            match entry {
            OpRes::Ret(e) => return Ok::<S::ValueRef<'s>, ExecError>(e),
            OpRes::TailCall(p) => entry_ref = p.get_value()?,
            _ => panic!("Unexpected!")
            }
        }
    }

    // try_force will check if (a) the object being forced is in
    // fact a thunk and (b) if someone else is already forcing this thunk
    // matching with other force task
    pub async fn force(&self, thunk_ref: &S::EntryRef<'s>) -> Result<(), ExecError> {
        // check if the it even is a pointer
        let thunk_lam_target = {
            let thunk_data = thunk_ref.get_value()?;
            match thunk_data.reader().thunk() {
                Some(s) => s,
                None => return Ok(())
            }
        };
        // check if the lambda is currently being forced
        // by looking into the force map
        let ptr = thunk_ref.ptr();
        let e = self.thunk_exec.borrow();
        match e.get(&ptr) {
            Some(h) => {
                h.clone().recv().await.unwrap();
                Ok(())
            },
            None => {
                std::mem::drop(e);
                let mut e = self.thunk_exec.borrow_mut();
                let (s, r) = broadcast::<()>(1);
                e.insert(ptr, r.clone());
                let future = async move {
                    let target = self.store.get(thunk_lam_target).unwrap();
                    let res = self.force_task(target).await;
                    thunk_ref.set_value(res.unwrap());
                    s.broadcast(()).await.unwrap();
                }.boxed_local();
                future.await;
                Ok(())
            }
        }
    }

    fn compute_match(&self, _val : ValueReader<'_>, _select : MatchReader<'_>) -> i64 {
        0
    }

    async fn exec_op<'t>(&'t self, op : OpReader<'t>, code: CodeReader<'t>, thunk_ex: &LocalExecutor<'t>,
                    regs: &'t Registers<'s, S>, queue: &'t ExecQueue) -> Result<OpRes<'s, S>, ExecError> {
        use OpWhich::*;
        match op.which()? {
            Ret(id) => {
                let val = regs.consume(id)?.get_value()?;
                return Ok(OpRes::Ret(val));
            },
            TailRet(id) => {
                // Tail-call into the entry
                let entry = regs.consume(id)?;
                return Ok(OpRes::TailCall(entry))
            },
            Force(r) => {
                let entry = regs.consume(r.get_arg())?;
                // spawn the force as a background task
                // since we might want to move onto other things
                thunk_ex.spawn(async move {
                    self.force(&entry).await.unwrap();
                    // we need to get 
                    regs.set_object(r.get_dest().unwrap(), entry).unwrap();
                    queue.complete(r.get_dest().unwrap(), code.reborrow()).unwrap();
                }).detach();
            },
            RecForce(_) => panic!("Not implemented"),
            Bind(r) => {
                let lam = regs.consume(r.get_lam())?;
                let lam_val = lam.get_value()?;
                let (code_entry, old_args) = match lam_val.reader().which()? {
                    ValueWhich::Code(_) => (lam, Vec::new()),
                    ValueWhich::Partial(p) => {
                        let p = p?;
                        let code = self.store.get(p.get_code().into())?;
                        // parse the existing args
                        let args : Result<Vec<S::EntryRef<'s>>, StorageError> = p.get_args()?.into_iter()
                                    .map(|x| self.store.get(x.into())).collect();
                        (code, args?)
                    },
                    _ => panic!()
                };
                let new_args : Result<Vec<S::EntryRef<'s>>, ExecError> = r.get_args()?.into_iter()
                            .map(|x| regs.consume(x)).collect();
                let mut new_args = new_args?;
                new_args.extend(old_args);
                // construct a new partial with the modified arguments
                let new_partial = self.store.insert_build(|b| {
                    let mut pb = b.init_partial();
                    pb.set_code(code_entry.ptr().raw());
                    let mut ab = pb.init_args(new_args.len() as u32);
                    for (i, v) in new_args.iter().enumerate() {
                        ab.set(i as u32, v.ptr().raw());
                    }
                    Ok(())
                })?;
                regs.set_object(r.get_dest()?, new_partial)?;
                queue.complete(r.get_dest()?, code.reborrow())?;
            },
            Invoke(r) => {
                let target_entry = regs.consume(r.get_src())?;
                let entry = self.store.insert_build(|mut root| {
                    root.set_thunk(target_entry.ptr().raw());
                    Ok(())
                })?;
                regs.set_object(r.get_dest()?, entry)?;
                queue.complete(r.get_dest()?, code.reborrow())?;
            },
            Builtin(r) => {
                let name = r.get_op()?;
                // consume the arguments
                let args : Result<Vec<S::EntryRef<'s>>, ExecError> = 
                    r.get_args()?.into_iter().map(|x| regs.consume(x)).collect();
                let args = args?;
                if builtin::is_sync(name) {
                    let e = builtin::sync_builtin(self, name, args)?;
                    regs.set_object(r.get_dest()?, e)?;
                    queue.complete(r.get_dest()?, code.reborrow())?;
                } else {
                    // if this is not a synchronous builtin,
                    // execute it asynchronously
                    thunk_ex.spawn(async move {
                        let entry = builtin::async_builtin(self, name, args).await.unwrap();
                        // we need to get 
                        regs.set_object(r.get_dest().unwrap(), entry).unwrap();
                        queue.complete(r.get_dest().unwrap(), code.reborrow()).unwrap();
                    }).detach();
                }
            },
            Match(r) => {
                // get the value we are supposed to be matching
                let scrut = regs.consume(r.get_scrut())?.get_value()?;
                // get the case of the value
                let case = self.compute_match(scrut.reader(), r.reborrow());
                let entry = self.store.insert_build(
                    |root| {
                        root.init_primitive().set_int(case);
                        Ok(())
                })?;
                regs.set_object(r.get_dest()?, entry)?;
                queue.complete(r.get_dest()?, code.reborrow())?;
            },
            Select(r) => {
                let branches : Result<Vec<S::EntryRef<'s>>, ExecError> = 
                    r.get_branches()?.into_iter().map(|x| regs.consume(x)).collect();
                let branches = branches?;

                let case = regs.consume(r.get_case())?;
                let case = case.get_value()?.reader().int()?;
                let opt = branches.into_iter().nth(case as usize)
                    .ok_or(ExecError {})?;
                // force the selected option
                self.force(&opt).await?;
                regs.set_object(r.get_dest()?, opt)?;
                queue.complete(r.get_dest()?, code.reborrow())?;
            }
        }
        Ok(OpRes::Continue)
    }
}