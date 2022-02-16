use crate::value::{
    Allocator
};

use super::op::{OpAddr, OpCount, ObjectID, CodeReader, DestReader, Dependent};
use super::ExecError;

use deadqueue::unlimited::Queue;
use std::collections::HashMap;
use slab::Slab;
use std::cell::RefCell;


// An execqueue manages the execution of a particular
// code block by tracking dependencies
// It needs to be shared among all ongoing coroutines
// being executed
pub struct ExecQueue {
    // The queue of operations that are
    // ready to execute
    queue : Queue<OpAddr>,
    // map from op to number of dependencies
    // left to be satisfied.
    waiting : RefCell<HashMap<OpAddr, OpCount>>,
}

impl ExecQueue {
    pub fn new() -> Self {
        Self { 
            queue: Queue::new(), 
            waiting: RefCell::new(HashMap::new())
        }
    }

    pub async fn next_op(&self) -> OpAddr {
        self.queue.pop().await
    }

    // Will complete a particular operation, getting each of the
    // dependents and notifying them that a dependency has been completed
    pub fn complete(&self, dest: DestReader<'_>, code: CodeReader<'_>) -> Result<(), ExecError> {
        let deps = dest.get_used_by()?;
        for d in deps.iter() {
            self.dep_complete_for(d, code)?;
        }
        Ok(())
    }

    // notify the execution queue that a dependency
    // for a given op was completed. This will release the
    // operation into the queue if all the dependencies have been
    // completed. If this is the first time the given operation
    // has a dependency complete, we read the operation and determine
    // the number of dependencies it has.
    fn dep_complete_for(&self, op: OpAddr, code: CodeReader<'_>) -> Result<(), ExecError> {
        let opr = code.get_ops()?.get(op);
        let mut w = self.waiting.borrow_mut();
        match w.get_mut(&op) {
            Some(r) => {
                *r = *r  - 1;
                if *r == 0 {
                    // release into the queue
                    w.remove(&op);
                    self.queue.push(op);
                }
            },
            None => {
                // this is the first time this op
                // is being listed as dependency complete, find the number of dependents
                let deps = opr.num_deps()?;
                if deps > 1 {
                    w.insert(op, deps - 1);
                } else {
                    self.queue.push(op);
                }
            }
        }
        Ok(())
    }
}

pub enum Reg<'s, S: Storage + 's> {
    Value(S::ObjectRef<'s>, OpCount), // the reference and usage count
    Temp(S::Indirect<'s>)
}


// Registers manages a map from data id --> underlying data
// as well as the consumption of data

// From the autoside perspective, this structure should *appear*
// as if it is atomic, so all methods take &
// (so &Registers can be shared among multiple ongoing operations). 
pub struct Registers<'s, S: Storage + 's> {
    // slab-allocated registers
    regs : RefCell<Slab<Reg<'s, S>>>,
    // map from ObjectID to the slab register key
    reg_map: RefCell<HashMap<ObjectID, usize>>,
    store: &'s S
}

impl<'s, S: Storage> Registers<'s, S> {
    pub fn new(store: &'s S) -> Self {
        Self {
            regs: RefCell::new(Slab::new()),
            reg_map: RefCell::new(HashMap::new()),
            store
        }
    }

    // Will set a particular ObjectID to a given entry value, as well as
    // a number of uses for this data until the register should be discarded
    pub fn set_object(&self, d: DestReader<'_>, e: S::ObjectRef<'s>) -> Result<(), ExecError> {
        // If there is a lifting allocation, that mapping
        // should have been removed using alloc_entry.
        // To ensure that is the case, we error if there is a mapping
        let id = d.get_id();
        let uses = d.get_used_by()?.len() as OpCount;

        let mut regs = self.regs.borrow_mut();
        let mut reg_map = self.reg_map.borrow_mut();
        match reg_map.get(&id)  {
            Some(r) => {
                // this register has already been set with an indirect tmp
                let r = regs.get_mut(*r).unwrap();
                // swap out the tmp indirect
                let e = match r {
                    Reg::Temp(t) => t.get_target(),
                    _ => return Err(ExecError::new("Tried to set object twice"))
                };
                // swap the temporary to a pointer to the
                // indirect object
                let mut nr = Reg::Value(e.clone(), uses);
                std::mem::swap(r, &mut nr);
                // now we remap the temporary
                match nr {
                    Reg::Temp(t) => { t.set(e).unwrap(); },
                    _ => panic!("Should not be reachable")
                }
            },
            None => {
                // just set the register as per normal
                let key = regs.insert(Reg::Value(e, uses));
                reg_map.insert(id, key);
            }
        }
        Ok(())
    }

    // Will get an entry, either (1) reducing the remaining uses
    // or (2) use an indirect
    pub fn consume(&self, d: ObjectID) -> Result<S::ObjectRef<'s>, ExecError> {
        let mut reg_map = self.reg_map.borrow_mut();
        let mut regs= self.regs.borrow_mut();

        let reg_idx = reg_map.get(&d).map(|x|*x);
        match reg_idx {
        None => {
            // Create a new lifting allocation, insert a copy into the registers
            // and also return it here.
            let tmp = self.store.indirection()?;
            let e = tmp.get_target();
            let key = regs.insert(Reg::Temp(tmp));
            reg_map.insert(d, key);
            Ok(e)
        },
        Some(idx) => {
            // There already exists an allocation
            let mut reg = regs.get_mut(idx).unwrap();
            let entry = match &mut reg {
                Reg::Value(e, uses) => {
                    *uses = *uses - 1;
                    if *uses == 0 {
                        // remove the entry and return the underlying ref
                        let reg = regs.remove(idx);
                        reg_map.remove(&d);
                        match reg { Reg::Value(e, _) => e, _ => panic!() }
                    } else {
                        e.clone()
                    }
                },
                Reg::Temp(t) => self.store.get(t.ptr())?
            };
            Ok(entry)
        }
        }
    }
}

pub fn populate<'s, S : Storage>(regs: &Registers<'s, S>, queue: &ExecQueue, code: CodeReader<'_>, 
                    args: Vec<S::ObjectRef<'s>>) 
                    -> Result<(), ExecError> {
    // setup the constants values
    for c in code.get_externals()?.iter() {
        regs.set_object(c.get_dest()?, regs.store.get(c.get_ptr().into())?)?;
        queue.complete(c.get_dest()?, code)?;
    }
    // setup the argument values
    for (e, dest) in args.into_iter().zip(code.get_params()?.iter()) {
        regs.set_object(dest, e)?;
        queue.complete(dest.reborrow(), code)?;
    }
    Ok(())
}