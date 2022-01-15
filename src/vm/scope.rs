use crate::value::{
    Storage,
};
use crate::value::storage::ValueEntry;
use super::op::{OpAddr, ValueID, CodeReader};
use super::machine::ExecError;

use deadqueue::unlimited::Queue;
use std::borrow::Borrow;
use std::collections::HashMap;
use slab::Slab;
use std::cell::RefCell;

// An execqueue manages the execution of a particular
// code block by tracking dependencies
// It needs to be shared among all ongoing coroutines
// being executed
pub struct ExecQueue<'sc> {
    // The queue of operations that are
    // ready to execute
    queue : Queue<OpAddr>,
    // map from op to number of dependencies
    // left to be satisfied.
    waiting : RefCell<HashMap<OpAddr, usize>>,
    code: RefCell<CodeReader<'sc>>
}

impl<'sc> ExecQueue<'sc> {
    pub fn new(code: CodeReader<'sc>) -> Self {
        Self {
            queue: Queue::new(), 
            waiting: RefCell::new(HashMap::new()),
            code: RefCell::new(code)
        }
    }

    pub async fn next_op(&self) -> OpAddr {
        self.queue.pop().await
    }

    // Will complete a particular operation, getting each of the
    // dependents and notifying them that a dependency has been completed
    pub fn complete(&self, op: OpAddr) -> Result<(), ExecError> {
        let r = self.code.borrow();
        let r = r.get_ops()?.get(op as u32);
    }

    // notify the execution queue that a dependency
    // for a given op was completed. This will release the
    // operation into the queue if all the dependencies have been
    // completed. If this is the first time the given operation
    // has a dependency complete, we read the operation and determine
    // the number of dependencies it has.
    fn dep_complete_for(&self, op: OpAddr) -> Result<(), ExecError> {
        let r = self.code.borrow();
        let r = r.get_ops()?.get(op);
    }
}

pub enum RegValue<'sc, S: Storage + 'sc> {
    Value(S::Entry<'sc>),
    Init(S::Init<'sc>)
}

pub struct Reg<'sc, S: Storage + 'sc> {
    value: RegValue<'sc, S>,
    lifetime: u16
}


// Registers manages a map from data id --> underlying data
// as well as the consumption of data

// From the autoside perspective, this structure should *appear*
// as if it is atomic, so all methods take &
// (so &Registers can be shared among multiple ongoing operations). 
pub struct Registers<'sc, S: Storage + 'sc> {
    // slab-allocated registers
    regs : RefCell<Slab<Reg<'sc, S>>>,
    // map from ValueID to the slab register key
    reg_map: RefCell<HashMap<ValueID, usize>>,
}

impl<'sc, S: Storage> Registers<'sc, S> {
    pub fn new() -> Self {
        Registers {
            regs: RefCell::new(Slab::new()),
            reg_map: RefCell::new(HashMap::new())
        }
    }

    // Will allocate an initializer for a given object
    // This will reuse an earlier allocation if the allocation has
    // been lifted, or create a new allocation if not.
    // Note that you still need to call *set_object* in order
    // to set this particular ValueID, this just manages creating the allocation
    // in a manner that handles recursive definitions
    pub fn alloc_data(&self, store: &'sc S, d: ValueID) -> Result<S::Init<'sc>, ExecError> {
        let reg_map = self.reg_map.borrow_mut();
        match reg_map.get(&d) {
            Some(reg_idx) => {
                // use the earlier allocation
                let regs= self.regs.borrow_mut();
                let reg = regs.try_remove(*reg_idx).ok_or(ExecError{})?;
                match reg.value {
                    RegValue::Init(i) => Ok(i),
                    _ => Err(ExecError{})
                }
            },
            None => Ok(store.alloc())
        }

    }

    // Will set a particular ValueID to a given entry value, as well as
    // a number of uses for this data until the register should be discarded
    pub fn set_value(&self, d: ValueID, e: S::Entry<'sc>, uses: usize) -> Result<(), ExecError> {

    }

    // For when we need the raw value
    pub fn consume_data(&self, d: ValueID) -> Result<S::Entry<'sc>, ExecError> {
        let reg_map = self.reg_map.borrow();
        let regs= self.regs.borrow_mut();

        let reg_idx = *reg_map.get(&d).ok_or(ExecError {})?;
        let reg = regs.get_mut(reg_idx).ok_or(ExecError {})?;
        match &reg.value {
            RegValue::Init(_) => Err(ExecError{}),
            RegValue::Value(v) => {
                reg.lifetime = reg.lifetime - 1;
                if reg.lifetime == 0 {
                    let RegValue::Value(v) = regs.remove(reg_idx).value;
                    Ok(v)
                } else {
                    Ok(v.clone())
                }
            }
        }
    }

    // For when we just need the pointer. If there is
    // no register associated with this Data ID, it will
    // allocate a pointer instead
    pub fn consume_data_ptr(&self, d: ValueID) -> Result<S::Entry<'sc>, ExecError> {
        let reg_map = self.reg_map.borrow();
        let regs= self.regs.borrow_mut();

        let reg_idx = *reg_map.get(&d).ok_or(ExecError {})?;
        let reg = regs.get(reg_idx).ok_or(ExecError {})?;
        match reg.value {
            RegValue::Init(_) => Err(ExecError{}),
            RegValue::Value(v) => {
                reg.lifetime = reg.lifetime - 1;
                if reg.lifetime == 0 {
                    *reg = RegValue::Empty;
                }
                Ok(v.ptr())
            }
        }
    }
}

/*
impl<'sc, S: Storage> Scope<'sc, S> {

    pub fn from_thunk<'e>(thunk: Pointer, 
                store: &'sc S) -> Result<Scope<'sc, S>, ExecError> {
        let te = store.get(thunk).ok_or(ExecError{})?;
        // instantiate a scope based on the reader for the thunk
        let tr = match te.reader().which()? {
            ValueWhich::Thunk(r) => Some(r),
            _ => None
        }.ok_or(ExecError{})?;
        // get the code associated with the thunk
        let code_ptr = Pointer::from(tr.get_lam());
        let code_entry = store.get(code_ptr).ok_or(ExecError{})?;
        let cr = match code_entry.reader().which()? {
            ValueWhich::Code(r) => Some(r?),
            _ => None
        }.ok_or(ExecError{})?;
        let params = cr.get_params()?;
        // match with the thunks
        let mut arg_types = tr.get_arg_types()?.iter();
        let mut arg_ptrs = tr.get_args()?.iter();

        // the initial registers, unpacked from the parameters
        let mut regs = Vec::new();
        for p in params.iter() {
            let ptr = match p.which()? {
                ParamWhich::Lift(_) => {
                    let t = arg_types.next().ok_or(ExecError{})??;
                    let a = Pointer::from(arg_ptrs.next().ok_or(ExecError{})?);
                    match t {
                    ApplyType::Lift => Ok(a),
                    _ => Err(ExecError{})
                    }?
                },
                ParamWhich::Pos(_) => {
                    let t = arg_types.next().ok_or(ExecError{})??;
                    let a = Pointer::from(arg_ptrs.next().ok_or(ExecError{})?);
                    match t {
                    ApplyType::Pos => Ok(a),
                    _ => Err(ExecError{})
                    }?
                }
            };
            regs.push(store.get(ptr).ok_or(ExecError{})?);
        }
        let scope = Scope::new(code_entry, regs);
        Ok(scope)
    }
}
*/