use crate::store::{Storage, PartialReader, StringReader};

use std::collections::HashMap;
use std::rc::Rc;
use crate::{Error, ErrorKind};
use crate::compile::Env;
use crate::store::{ObjectType, ObjectReader, CodeReader, 
                    RecordReader, Handle, ReaderWhich};
use crate::store::op::{Op, OpAddr};
use crate::store::value::Value;
use crate::store::print::Depth;

use super::trace::{Cache, Lookup, TraceBuilder};
use super::scope::{ExecQueue, Registers};

use std::borrow::Borrow;
use futures_lite::future::BoxedLocal;
use smol::LocalExecutor;
use pretty::{BoxAllocator, BoxDoc};

pub trait SyscallHandler<'s, 'c, C: Cache<'s, S>, S : Storage + 's> {
    fn call(&self, mach: &Machine<'s, 'c, C, S>, args: Vec<S::Handle<'s>>)
        -> BoxedLocal<Result<S::Handle<'s>, Error>>;
}

pub struct Machine<'s, 'c, C: Cache<'s, S>, S: Storage> {
    pub store: &'s S, 
    pub cache: &'c C,
    pub syscalls: HashMap<String, Rc<dyn SyscallHandler<'s, 'c, C, S>>>
}

impl<'s, 'c, C: Cache<'s, S>, S: Storage> Machine<'s, 'c, C, S> {
    pub fn new(store: &'s S, cache: &'c C) -> Self {
        Self { 
            store, cache,
            syscalls: HashMap::new()
        }
    }

    pub fn add_syscall<O: ToOwned<Owned=String>>(&mut self, sys: O, handler: Rc<dyn SyscallHandler<'s, 'c, C, S>>) {
        self.syscalls.insert(sys.to_owned(), handler);
    }

    pub async fn env_use(&self, mod_ref: S::Handle<'s>, env: &mut Env<S::Handle<'s>>) -> Result<(), Error> {
        let handle = self.force(mod_ref).await?;
        let reader = handle.reader()?;
        let rec = reader.as_record()?;
        for (k, v) in rec.iter() {
            let key_reader = k.borrow().reader()?;
            let key_str = key_reader.as_string()?;
            let value = v.borrow().clone();
            env.insert(key_str.as_slice().to_owned(), value);
        }
        Ok(())
    }

    // Does the actual forcing in a loop, and checks the trace cache first
    pub async fn force(&self, thunk_ref: S::Handle<'s>)
            -> Result<S::Handle<'s>, Error> {
        let mut thunk_ref = thunk_ref;
        loop {
            log::trace!(target: "vm", "trying {}", thunk_ref);
            // first check the cache for this thunk
            let _type = thunk_ref.reader()?.get_type();
            if ObjectType::Thunk != _type {
                log::trace!(target: "vm", "{} is already WHNF of type {:?}", thunk_ref, _type);
                return Ok(thunk_ref)
            }
            // check the cache for this particular thunk
            let next_thunk = {
                let query_res = self.cache.query(self, &thunk_ref).await?;
                match query_res {
                    Lookup::Hit(v) => {
                        log::trace!(target: "vm", "hit &{}", thunk_ref);
                        v
                    },
                    Lookup::Miss(trace, _) => {
                        log::trace!(target: "vm", "miss &{}", thunk_ref);
                        let res = self.force_stack(thunk_ref.clone()).await?;
                        trace.returned(res.clone());
                        res
                    }
                }
            };
            thunk_ref = next_thunk;
            if ObjectType::Thunk != thunk_ref.reader()?.get_type() {
                return Ok(thunk_ref)
            }
        }
    }

    // Does a single stack worth of forcing (and returns)
    async fn force_stack(&self, thunk_ref: S::Handle<'s>) -> Result<S::Handle<'s>, Error> {
        // get the entry ref 
        let entry_ref = thunk_ref.reader()?.as_thunk()?;
        let (code_ref, inputs) = match entry_ref.borrow().reader()?.which() {
            ReaderWhich::Code(_) => (entry_ref.borrow().clone(), Vec::new()),
            ReaderWhich::Partial(p) => {
                (p.get_code().borrow().clone(), p.iter_args().map(|x| x.borrow().clone()).collect())
            },
            _ => return Err(Error::new_const(ErrorKind::Internal, "Force target is not code or a partial"))
        };
        let code_reader = code_ref.reader()?.as_code()?;
        let queue = ExecQueue::new();
        let regs = Registers::new(self.store, code_reader.get_ret());

        {
            let code_doc: BoxDoc<'_, ()> = code_reader.pretty(Depth::Fixed(2), &BoxAllocator).into_doc();
            log::trace!(target: "vm", "executing:\n{}", code_doc.pretty(80));
        }

        for op_addr in code_reader.iter_ready() {
            queue.push(op_addr);
        }

        log::trace!(target: "vm", "populated");

        // We need to drop the local executor before the queue, regs
        let thunk_ex = LocalExecutor::new();
        let res : Result<S::Handle<'s>, Error> = thunk_ex.run(async {
            loop {
                let addr : OpAddr = queue.next_op().await;
                let op = code_reader.get_op(addr);
                log::trace!(target: "vm", "executing #{} for {} (code {}): {}", addr, thunk_ref, code_ref, op);
                self.exec_op(op, &code_reader, &thunk_ex, &regs, &queue, &inputs)?;
                match regs.return_value() {
                    Some(h) => return Ok(h),
                    None => ()
                }
            }
        }).await;
        log::trace!(target: "vm", "done");
        res
    }

    fn exec_op<'t, 'r, R: CodeReader<'r, 's, Handle=S::Handle<'s>>>(&'t self, op : Op, code: &'t R, thunk_ex: &LocalExecutor<'t>,
                    regs: &'t Registers<'s, S>, queue: &'t ExecQueue, inputs: &Vec<S::Handle<'s>>) -> Result<(), Error> {
        use Op::*;
        match op {
            Force(dest, arg) => {
                let entry = regs.consume(arg)?;
                if entry.reader()?.get_type() == ObjectType::Thunk {
                    // spawn the force as a background task
                    // since we might want to move onto other things
                    thunk_ex.spawn(async move {
                        let res = self.force(entry).await.unwrap();
                        // we need to get 
                        regs.set_object(&dest, res).unwrap();
                        queue.complete(&dest, code).unwrap();
                    }).detach();
                } else {
                    // We are already WHNF
                    regs.set_object(&dest, entry)?;
                    queue.complete(&dest, code)?;
                }
            },
            SetValue(dest, value) => {
                let h = code.get_value(value).unwrap();
                regs.set_object(&dest, h.borrow().clone())?;
                queue.complete(&dest, code)?;
            },
            SetInput(dest, input) => {
                let val = inputs[input as usize].clone();
                regs.set_object(&dest, val)?;
                queue.complete(&dest, code)?;
            },
            Bind(dest, lam, bind_args) => {
                let lam = regs.consume(lam)?;
                let (code_entry, mut args) = match lam.reader()?.get_type() {
                    ObjectType::Code => (lam.clone(), Vec::new()),
                    ObjectType::Partial => {
                        let partial = lam.reader()?.as_partial()?;
                        let code = partial.get_code().borrow().clone();
                        let args = partial.iter_args().map(|x| x.borrow().clone()).collect();
                        (code, args)
                    },
                    _ => return Err(Error::new_const(ErrorKind::Internal, "Can only bind to a code or partial"))
                };
                let new_args : Result<Vec<S::Handle<'s>>, Error> = 
                    bind_args.iter().map(|x| regs.consume(*x)).collect();
                args.extend(new_args?);
                // construct a new partial with the modified arguments
                let res = self.store.insert_from(&Value::Partial(code_entry, args))?;
                regs.set_object(&dest, res)?;
                queue.complete(&dest, code)?;
            },
            Invoke(dest, target) => {
                let target_entry = regs.consume(target)?;
                let entry = self.store.insert_from(&Value::Thunk(target_entry))?;
                regs.set_object(&dest, entry)?;
                queue.complete(&dest, code)?;
            },
            Builtin(_dest, _op, _args) => {
                // builtin::exec_builtin(self, r, code, thunk_ex, regs, queue)?;
                todo!()
            },
            Match(_dest, _scrut, _cases) => {
                todo!()
                // // get the value we are supposed to be matching
                // let scrut = regs.consume(r.get_scrut())?;
                // // get the case of the value
                // let opt = self.compute_match(&scrut, r.reborrow())?;
                // regs.set_object(r.get_dest()?, opt)?;
                // queue.complete(r.get_dest()?, code.reborrow())?;
            },
        }
        Ok(())
    }
}