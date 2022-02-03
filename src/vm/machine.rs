use super::op::{OpWhich, OpReader, OpAddr, CodeReader, MatchReader};
use super::builtin;
use smol::LocalExecutor;
use crate::value::{
    ExtractValue,
    ValueWhich,
    ValueReader
};

use crate::value::storage::{
    Storage, StorageError,
    ObjPointer, ObjectRef, ValueRef
};
use super::{scope, scope::{Registers, ExecQueue}};
use super::ExecError;
use super::tracer::{ExecCache, Lookup, TraceBuilder};

pub type RegAddr = u16;

pub struct Machine<'s, 'e, S: Storage,
                   E : ExecCache<'s, S> + ?Sized> {
    // the storage must be multi &-safe, but does not need to be threading safe
    pub store: &'s S, 
    // The cache of what is currently executing.
    // This also manages the immutable global variable,
    // ensuring that for the entirety of the machine execution
    // we use the same global values. Updating the globals (say due to file change) requires
    // instantiating a new machine
    pub cache: &'e E
}

enum OpRes<'s, S: Storage + 's> {
    Continue,
    Ret(S::ObjectRef<'s>), // the object whose value to copy into the original thunk
    ForceRet(S::ObjectRef<'s>) // The thunk to tail-call into
}

impl<'s, 'e, S: Storage, E : ExecCache<'s, S>> Machine<'s, 'e, S, E> {
    pub fn new(store: &'s S, cache: &'e E) -> Self {
        Self { 
            store, cache
        }
    }

    // Does the actual forcing in a loop, and checks the trace cache first
    pub async fn force(&self, thunk_ref: S::ObjectRef<'s>) -> Result<S::ObjectRef<'s>, ExecError> {
        // first check the cache for this thunk
        let mut thunk_ref = thunk_ref;
        loop {
            // check the cache for this particular thunk
            let next_thunk = {
                let query_res = self.cache.query(self, &thunk_ref).await?;
                match query_res {
                    Lookup::Hit(v) => return Ok(v),
                    Lookup::Miss(trace, _) => {
                        let res = self.force_stack(&thunk_ref).await?;
                        match res {
                            OpRes::Ret(val) => {
                                trace.returned(val.clone());
                                return Ok(val)
                            },
                            OpRes::ForceRet(next_thunk) => {
                                next_thunk
                            },
                            OpRes::Continue => panic!("Should be unreachable!")
                        }
                    }
                }
            };
            thunk_ref = next_thunk;
        }
    }

    // Does a single stack worth of forcing (and returns)
    async fn force_stack(&self, thunk_ref: &S::ObjectRef<'s>) -> Result<OpRes<'s, S>, ExecError> {
        // get the entry ref 
        let entry_ref = self.store.get(
            thunk_ref.value()?.reader().thunk().ok_or(ExecError {})?
        )?;
        let (code_obj, args) = match entry_ref.value()?.reader().which()? {
            ValueWhich::Code(_) => (entry_ref.clone(), Vec::new()),
            ValueWhich::Partial(r) => {
                let r = r?;
                let code_ref = self.store.get(ObjPointer::from(r.get_code()))?;
                let args : Result<Vec<S::ObjectRef<'s>>, StorageError> = r.get_args()?.into_iter()
                            .map(|x| self.store.get(ObjPointer::from(x))).collect();
                (code_ref, args?)
            },
            _ => return Err(ExecError {})
        };
        let code_value = code_obj.value()?;
        let code_reader = code_value.reader().code().ok_or(ExecError {})?;
        let queue = ExecQueue::new();
        let regs = Registers::new(self.store);

        scope::populate(&regs, &queue, code_reader, args)?;

        // We need to drop the local executor before the queue, regs
        let thunk_ex = LocalExecutor::new();
        Ok(thunk_ex.run(async {
            loop {
                let addr : OpAddr = queue.next_op().await;
                let op = code_reader.get_ops()?.get(addr as u32);
                let res = self.exec_op(op, code_reader.reborrow(), &thunk_ex, &regs, &queue).await;

                #[cfg(test)]
                println!("[vm] executing {} for {}", addr, thunk_ref.ptr().raw());
                match res? {
                    OpRes::Continue => {},
                    OpRes::Ret(r)  => {
                        return Ok::<OpRes<'s, S>, ExecError>(OpRes::Ret(r))
                    }
                    OpRes::ForceRet(r) => {
                        return Ok::<OpRes<'s, S>, ExecError>(OpRes::ForceRet(r))
                    }
                }
            }
        }).await?)
    }

    fn compute_match(&self, _val : ValueReader<'_>, _select : MatchReader<'_>) -> i64 {
        0
    }

    async fn exec_op<'t>(&'t self, op : OpReader<'t>, code: CodeReader<'t>, thunk_ex: &LocalExecutor<'t>,
                    regs: &'t Registers<'s, S>, queue: &'t ExecQueue) -> Result<OpRes<'s, S>, ExecError> {
        use OpWhich::*;
        match op.which()? {
            Ret(id) => {
                let val = regs.consume(id)?;
                return Ok(OpRes::Ret(val));
            },
            ForceRet(id) => {
                // Tail-call into the thunk
                let thunk = regs.consume(id)?;
                return Ok(OpRes::ForceRet(thunk))
            },
            Force(r) => {
                let entry = regs.consume(r.get_arg())?;
                // spawn the force as a background task
                // since we might want to move onto other things
                thunk_ex.spawn(async move {
                    let res = self.force(entry).await.unwrap();
                    // we need to get 
                    regs.set_object(r.get_dest().unwrap(), res).unwrap();
                    queue.complete(r.get_dest().unwrap(), code.reborrow()).unwrap();
                }).detach();
            },
            RecForce(_) => panic!("Not implemented"),
            Bind(r) => {
                let lam = regs.consume(r.get_lam())?;
                let lam_val = lam.value()?;
                let (code_entry, old_args) = match lam_val.reader().which()? {
                    ValueWhich::Code(_) => (lam, Vec::new()),
                    ValueWhich::Partial(p) => {
                        let p = p?;
                        let code = self.store.get(p.get_code().into())?;
                        // parse the existing args
                        let args : Result<Vec<S::ObjectRef<'s>>, StorageError> = p.get_args()?.into_iter()
                                    .map(|x| self.store.get(x.into())).collect();
                        (code, args?)
                    },
                    _ => panic!()
                };
                let new_args : Result<Vec<S::ObjectRef<'s>>, ExecError> = r.get_args()?.into_iter()
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
                let args : Result<Vec<S::ObjectRef<'s>>, ExecError> = 
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
                let scrut = regs.consume(r.get_scrut())?;
                let scrut = scrut.value()?;
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
                let branches : Result<Vec<S::ObjectRef<'s>>, ExecError> = 
                    r.get_branches()?.into_iter().map(|x| regs.consume(x)).collect();
                let branches = branches?;

                let case = regs.consume(r.get_case())?;
                let case = case.value()?.reader().int()?;
                let opt = branches.into_iter().nth(case as usize)
                    .ok_or(ExecError {})?;

                // since we are doing a force, this needs
                // to run in the background
                thunk_ex.spawn(async move {
                    // force the selected option
                    let res = self.force(opt).await.unwrap();
                    regs.set_object(r.get_dest().unwrap(), res).unwrap();
                    queue.complete(r.get_dest().unwrap(), code.reborrow()).unwrap();
                }).detach();
            }
        }
        Ok(OpRes::Continue)
    }
}