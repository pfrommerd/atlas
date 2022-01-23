use super::op::{OpWhich, OpReader, OpAddr, ArgWhich, CodeReader};
use std::cell::RefCell;
use super::builtin;
use futures_lite::FutureExt;
use smol::LocalExecutor;
use async_broadcast::broadcast;
// use super::op::{CodeReader, RegAddr, OpAddr, Op, OpPrimitive};
use crate::value::{
    ExtractValue,
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

    async fn extract_base(&self, entry_ptr: ObjPointer)
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
                self.force(&thunk).await?;
                code = thunk;
            },
            _ => return Err(ExecError {})
            };
        }
        Ok((code.ptr(), closure, apply_bkw))
    }

    async fn force_task<'e>(&'e self, mut entry_ref: S::EntryRef<'s>) -> Result<S::ValueRef<'s>, ExecError> {
        loop { // loop for tail call recursion
            let queue = ExecQueue::new();
            let regs = Registers::new(self.store);
            let (code_ptr, closure, args) =
                self.extract_base(entry_ref.ptr()).await?;
            // setup the pointers and get a reference to the code entry
            let code_data = self.store.get_value(code_ptr)?;
            let code_reader = code_data.reader().code().ok_or(ExecError {})?;
            regs.populate(&queue, code_reader, &closure, &args)?;

            // We need to drop the local executor before the code
            let thunk_ex = LocalExecutor::new();
            let entry : OpRes<'s, S> = thunk_ex.run(async {
                loop {
                    let addr : OpAddr = queue.next_op().await;
                    println!("Executing {addr}");
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
            OpRes::TailCall(p) => entry_ref = p,
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
            Closure(r) => { // construct a closure
                let entry : Result<S::EntryRef<'s>, ExecError> = self.store.insert_build(|root| {
                    let closure_code = regs.consume(r.get_code())?;
                    let entries : Result<Vec<S::EntryRef<'s>>, ExecError> = r.get_entries()?.into_iter()
                                .map(|x| regs.consume(x)).collect();
                    let entries = entries?;
                    let mut closure = root.init_closure();
                    closure.set_code(closure_code.ptr().raw());
                    let mut e = closure.init_entries(entries.len() as u32);
                    for (i, r) in entries.into_iter().enumerate() {
                        e.set(i as u32, r.ptr().raw())
                    }
                    Ok(())
                });
                regs.set_object(r.get_dest()?, entry?)?;
                queue.complete(r.get_dest()?, code.reborrow())?;
            },
            Apply(r) => {
                let lam = regs.consume(r.get_lam())?;
                let entry : Result<S::EntryRef<'s>, ExecError> = self.store.insert_build(|root| {
                    let mut apply = root.init_apply();
                    apply.set_lam(lam.ptr().raw());
                    let args = r.get_args()?.iter();
                    let mut args_builder = apply.reborrow().init_args(args.len() as u32);
                    for (i, a) in args.enumerate() {
                        let v = regs.consume(a.get_val())?;
                        let mut ab = args_builder.reborrow().get(i as u32);
                        ab.set_val(v.ptr().raw());
                        // setup the argument value (consuming from the registers)
                        match a.which()? {
                            ArgWhich::Pos(_) => ab.set_pos(()),
                            ArgWhich::Key(k) => ab.set_key(regs.consume(k)?.ptr().raw()),
                            ArgWhich::VarPos(_) => ab.set_var_pos(()),
                            ArgWhich::VarKey(_) => ab.set_var_key(())
                        }
                    }
                    Ok(())
                });
                regs.set_object(r.get_dest()?, entry?)?;
                queue.complete(r.get_dest()?, code.reborrow())?;
            },
            Invoke(r) => {
                let entry : Result<S::EntryRef<'s>, ExecError> = self.store.insert_build(|mut root| {
                    root.set_thunk(regs.consume(r.get_src())?.ptr().raw());
                    Ok(())
                });
                regs.set_object(r.get_dest()?, entry?)?;
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
        }
        Ok(OpRes::Continue)
    }
}