use super::op::{OpWhich, OpReader, OpAddr, CodeReader, MatchReader};
use super::builtin;
use smol::LocalExecutor;
use crate::value::{
    ObjHandle, Allocator, OwnedValue, ValueType
};
use super::{scope, scope::{Registers, ExecQueue}};
use super::ExecError;
use super::tracer::{ExecCache, Lookup, TraceBuilder};

pub type RegAddr = u16;

pub struct Machine<'a, 'e, A: Allocator,
                   E : ExecCache<'a, A> + ?Sized> {
    // the storage must be multi &-safe, but does not need to be threading safe
    pub alloc: &'a A, 
    // The cache of what is currently executing.
    // This also manages the immutable global variable,
    // ensuring that for the entirety of the machine execution
    // we use the same global values. Updating the globals (say due to file change) requires
    // instantiating a new machine
    pub cache: &'e E
}

enum OpRes<'a, A: Allocator> {
    Continue,
    Ret(ObjHandle<'a, A>), // the object whose value to copy into the original thunk
    ForceRet(ObjHandle<'a, A>) // The thunk to tail-call into
}

impl<'a, 'e, A: Allocator, E : ExecCache<'a, A>> Machine<'a, 'e, A, E> {
    pub fn new(alloc: &'a A, cache: &'e E) -> Self {
        Self { 
            alloc, cache
        }
    }

    // Does the actual forcing in a loop, and checks the trace cache first
    pub async fn force(&self, thunk_ref: ObjHandle<'a, A>)
            -> Result<ObjHandle<'a, A>, ExecError> {
        let mut thunk_ref = thunk_ref;
        loop {
            println!("[vm] trying &{}", thunk_ref.ptr());
            // first check the cache for this thunk
            if ValueType::Thunk == thunk_ref.get_type()? {
                println!("[vm] &{} is already WHNF", thunk_ref);
                return Ok(thunk_ref)
            }
            // check the cache for this particular thunk
            let next_thunk = {
                let query_res = self.cache.query(self, &thunk_ref).await?;
                match query_res {
                    Lookup::Hit(v) => {
                        println!("[vm] hit &{}", thunk_ref);
                        return Ok(v)
                    },
                    Lookup::Miss(trace, _) => {
                        println!("[vm] miss &{}", thunk_ref);
                        let res = self.force_stack(thunk_ref.clone()).await?;
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
    async fn force_stack(&self, thunk_ref: ObjHandle<'a, A>) -> Result<OpRes<'a, A>, ExecError> {
        // get the entry ref 
        let entry_ref = thunk_ref.as_thunk()?;
        let (code_ref, args) = match entry_ref.to_owned()? {
            OwnedValue::Code(_) => (entry_ref.clone(), Vec::new()),
            OwnedValue::Partial(code_handle, args) => {
                (code_handle, args)
            },
            _ => return Err(ExecError::new("Force target is not code or a partial"))
        };
        let code_value = code_ref.as_code()?;
        let code_reader = code_value.reader();
        let queue = ExecQueue::new();
        let regs = Registers::new(self.alloc);

        println!("[vm] executing:\n{}", code_reader);

        scope::populate(&regs, &queue, code_reader, args)?;

        // We need to drop the local executor before the queue, regs
        let thunk_ex = LocalExecutor::new();
        Ok(thunk_ex.run(async {
            loop {
                let addr : OpAddr = queue.next_op().await;
                let op = code_reader.get_ops()?.get(addr as u32);
                let res = self.exec_op(op, code_reader.reborrow(), &thunk_ex, &regs, &queue).await;

                println!("[vm] executing #{} for {} (code {}): {}", addr, thunk_ref, code_ref, op);
                match res? {
                    OpRes::Continue => {},
                    OpRes::Ret(r)  => {
                        return Ok::<OpRes<'a, A>, ExecError>(OpRes::Ret(r))
                    }
                    OpRes::ForceRet(r) => {
                        return Ok::<OpRes<'a, A>, ExecError>(OpRes::ForceRet(r))
                    }
                }
            }
        }).await?)
    }

    fn compute_match(&self, _val : &ObjHandle<'a, A>, _select : MatchReader<'_>) -> Result<ObjHandle<'a, A>, ExecError> {
        todo!()
    }

    async fn exec_op<'t>(&'t self, op : OpReader<'t>, code: CodeReader<'t>, thunk_ex: &LocalExecutor<'t>,
                    regs: &'t Registers<'a, A>, queue: &'t ExecQueue) -> Result<OpRes<'a, A>, ExecError> {
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
            Bind(r) => {
                let lam = regs.consume(r.get_lam())?;
                let (code_entry, mut args) = match lam.get_type()? {
                    ValueType::Code => (lam.clone(), Vec::new()),
                    ValueType::Partial => {
                        match lam.to_owned()? {
                        OwnedValue::Partial(c, a) => (c, a),
                        _ => panic!("Unexpected")
                        }
                    },
                    _ => return Err(ExecError::default())
                };
                let new_args : Result<Vec<ObjHandle<'a, A>>, ExecError> = 
                    r.get_args()?.into_iter().map(|x| regs.consume(x)).collect();
                args.extend(new_args?);
                // construct a new partial with the modified arguments
                let res = OwnedValue::Partial(code_entry, args).pack_new(self.alloc)?;
                regs.set_object(r.get_dest()?, res)?;
                queue.complete(r.get_dest()?, code.reborrow())?;
            },
            Invoke(r) => {
                let target_entry = regs.consume(r.get_src())?;
                let entry = OwnedValue::Thunk(target_entry).pack_new(self.alloc)?;
                regs.set_object(r.get_dest()?, entry)?;
                queue.complete(r.get_dest()?, code.reborrow())?;
            },
            Builtin(r) => {
                let name = r.get_op()?;
                // consume the arguments
                let args : Result<Vec<ObjHandle<'a, A>>, ExecError> = 
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
                // get the case of the value
                let opt = self.compute_match(&scrut, r.reborrow())?;
                regs.set_object(r.get_dest()?, opt)?;
                queue.complete(r.get_dest()?, code.reborrow())?;
            },
        }
        Ok(OpRes::Continue)
    }
}