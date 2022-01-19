use super::op::{OpReader, OpWhich, OpAddr};
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
                code = self.store.get(ap.get_lam().into())?;
                self.try_force(ex, &code).await?;
            },
            _ => return Err(ExecError {})
            };
        }
        Ok((code.ptr(), closure, apply_bkw))
    }

    async fn force_task<'e>(&'e self, ex: &'e LocalExecutor<'e>, entry_ptr: ObjPointer)
                                    -> Result<S::EntryRef<'s>, ExecError> {
        let (code_ptr, closure, args) = self.extract_base(ex, entry_ptr).await?;
        let code_data = self.store.get_data(code_ptr, self.data)?;
        let code_reader = code_data.value().code().ok_or(ExecError{})?;

        // create the queue, registers, and local executor
        let queue = ExecQueue::new(code_reader.reborrow());
        // this will set up the constants, arguments, and populate the queue
        let regs = Registers::new(self.store, &queue,
                                    code_reader.reborrow(), &closure, &args)?;
        let q = &queue;
        let r = &regs;
        // We need to drop the executor before everything else
        {
        let queue_exec = LocalExecutor::new();
        // run the local executor until we have finished all
        // of the operations
        let res = queue_exec.run(async {
            loop {
                let addr : OpAddr = queue.next_op().await;
                let op = code_reader.get_ops()?.get(addr as u32);
                // spawn the op execution into the queue executor
                queue_exec.spawn(async move {
                    self.run_op(ex, q, r, op).await.unwrap();
                }).detach();
            }
        }).await;
        res
        }
    }

    // try_force will check if (a) the object being forced is in
    // fact a thunk and (b) if someone else is already forcing this thunk
    // matching with other force task
    pub async fn try_force<'e>(&'e self, ex: &'e LocalExecutor<'e>, thunk_ref: &S::EntryRef<'_>) -> Result<(), ExecError> {
        // check if the it even is a pointer
        let thunk_data= thunk_ref.get_data(self.data)?;
        let thunk_lam_target = match thunk_data.value().thunk() {
            Some(s) => s,
            None => return Ok(())
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
            ex.spawn(async move { 
                let res = self.force_task(ex, thunk_lam_target).await;
                // Redirect ptr to the associated force task
                let res_data = match res {
                    Ok(res_ptr) => 
                        match res_ptr.get_data(self.data) {
                            Ok(d) => Ok(d),
                            Err(s) => Err(ExecError::from(s))
                        },
                    Err(e) => Err(e)
                };
                match res_data {
                    Ok(res_ptr) => {
                        self.store.get(ptr).unwrap().set_value(res_ptr.ptr());
                    },
                    Err(_) => panic!("Canot handle error")
                };
                s.broadcast(()).await.unwrap();
            }).detach();
            Some(r)
        }).ok_or(ExecError{})?;
        // while we are waiting for the child scope, we
        // don't hold any references
        h.recv().await.unwrap();
        Ok(())
    }

    // If the op can be executed immediately, it should just be executed and the queue notified
    // that the op is complete. If the op
    // 
    async fn run_op<'e>(&'e self, ex: &'e LocalExecutor<'e>,
                                queue: &ExecQueue<'_>, regs: &Registers<'s, S>, op: OpReader<'_>)
                                    -> Result<(), ExecError> {
        use OpWhich::*;
        match op.which()? {
            Force(r) => {
                let entry = regs.consume(r.get_arg())?;
                self.try_force(ex, &entry).await?;
                // set the result to be the same as the input
                // and release anyone waiting for the result
                regs.set_object(r.get_dest()?, entry)?;
                queue.complete(r.get_dest()?)?;
                Ok(())
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