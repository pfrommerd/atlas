use crate::{Error};
use crate::store::op::{OpAddr, OpCount, RegID, Dest};
use crate::store::{CodeReader, Storage, IndirectBuilder};

use deadqueue::unlimited::Queue;
use std::collections::HashMap;
use slab::Slab;
use std::cell::RefCell;

use std::ops::{Deref, DerefMut};

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

    pub fn push(&self, addr: OpAddr) {
        self.queue.push(addr)
    }

    pub async fn next_op(&self) -> OpAddr {
        self.queue.pop().await
    }

    // Will complete a particular operation, getting each of the
    // dependents and notifying them that a dependency has been completed
    pub fn complete<'p, 's, R: CodeReader<'p, 's>>(&self, dest: &Dest, code: &R) {
        for d in &dest.uses {
            self.dep_complete_for(*d, code);
        }
    }

    // notify the execution queue that a dependency
    // for a given op was completed. This will release the
    // operation into the queue if all the dependencies have been
    // completed. If this is the first time the given operation
    // has a dependency complete, we read the operation and determine
    // the number of dependencies it has.
    fn dep_complete_for<'p, 's, R: CodeReader<'p, 's>>(&self, op: OpAddr, code: &R) {
        let opr = code.get_op(op);
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
                let deps = opr.num_deps();
                if deps > 1 {
                    w.insert(op, deps - 1);
                } else {
                    self.queue.push(op);
                }
            }
        }
    }
}

pub enum Reg<'s, S: Storage + 's> {
    Value(S::Handle<'s>, OpCount), // the reference and usage count
    Temp(S::IndirectBuilder<'s>)
}

impl<'s, S: Storage + 's> Reg<'s, S> {
    fn use_temp(&mut self, handle: S::Handle<'s>, uses: OpCount) {
        let h = match self {
            Self::Temp(s) => s.handle(),
            _ => panic!()
        };
        let mut v = Reg::Value(h, uses);
        std::mem::swap(&mut v, self);
        match v {
            Self::Temp(s) => s.build(handle),
            _ => panic!()
        };
    }
}


// Registers manages a map from data id --> underlying data
// as well as the consumption of data

// From the autoside perspective, this structure should *appear*
// as if it is atomic, so all methods take &
// (so &Registers can be shared among multiple ongoing operations). 
pub struct Registers<'s, S: Storage> {
    // slab-allocated registers
    regs : RefCell<Slab<Reg<'s, S>>>,
    // map from ObjectID to the slab register key
    reg_map: RefCell<HashMap<RegID, usize>>,
    return_reg: RegID,
    return_value: RefCell<Option<S::Handle<'s>>>,
    error_value: RefCell<Option<Error>>,
    store: &'s S
}

impl<'s, S: Storage> Registers<'s, S> {
    pub fn new(store: &'s S, return_reg: RegID) -> Self {
        Self {
            regs: RefCell::new(Slab::new()),
            reg_map: RefCell::new(HashMap::new()),
            return_reg,
            return_value: RefCell::new(None),
            error_value: RefCell::new(None),
            store
        }
    }

    pub fn return_value(&self) -> Option<S::Handle<'s>> {
        let ret = self.return_value.borrow();
        match ret.deref() {
            Some(s) => Some(s.clone()),
            None => None
        }
    }

    pub fn error_value(&self) -> Option<Error> {
        let mut o = None;
        let mut rv = self.error_value.borrow_mut();
        std::mem::swap(&mut o, rv.deref_mut());
        o
    }

    // Will set a particular ObjectID to a given entry value, as well as
    // a number of uses for this data until the register should be discarded
    pub fn set_object(&self, dest: &Dest, e: S::Handle<'s>) {
        // If there is a lifting allocation, that mapping
        // should have been removed using alloc_entry.
        // To ensure that is the case, we error if there is a mapping
        let id = dest.reg;
        let uses = dest.uses.len() as OpCount;

        let mut regs = self.regs.borrow_mut();
        let mut reg_map = self.reg_map.borrow_mut();

        // If this is the return register, set the return flag
        if self.return_reg == id {
            let mut r = self.return_value.borrow_mut();
            *r = Some(e.clone())
        }
        match reg_map.get(&id)  {
            Some(r) => {
                // this register has already been set with an indirect tmp
                let r = regs.get_mut(*r).unwrap();
                r.use_temp(e, uses);
            },
            None => {
                // just set the register as per normal
                let key = regs.insert(Reg::Value(e, uses));
                reg_map.insert(id, key);
            }
        }
    }

    pub fn set_result(&self, dest: &Dest, e: Result<S::Handle<'s>, Error>) {
        match e {
            Ok(h) => self.set_object(dest, h),
            Err(e) => {
                let mut rv = self.error_value.borrow_mut();
                *rv = Some(e)
            }
        }
    }

    // Will get an entry, either (1) reducing the remaining uses
    // or (2) use an indirect
    pub fn consume(&self, d: RegID) -> Result<S::Handle<'s>, Error> {
        let mut reg_map = self.reg_map.borrow_mut();
        let mut regs= self.regs.borrow_mut();

        let reg_idx = reg_map.get(&d).map(|x|*x);
        match reg_idx {
        None => {
            // Insert a bot value. This will be replaced when the value is actually populated
            let tmp = self.store.indirect()?;
            let handle = tmp.handle();
            let key = regs.insert(Reg::Temp(tmp));
            reg_map.insert(d, key);
            Ok(handle)
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
                Reg::Temp(t) => t.handle()
            };
            Ok(entry)
        }
        }
    }
}