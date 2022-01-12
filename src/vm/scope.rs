use crate::value::{
    Storage, Pointer,
    ValueWhich, ParamWhich, ApplyType,
};
use crate::value::storage::ValueEntry;
use super::op::{OpAddr, OpReader, RegAddr, CodeReader};
use super::machine::ExecError;

pub struct Scope<'sc, S: Storage + 'sc> {
    // pointers into the heap
    regs : Vec<S::Entry<'sc>>,
    code: S::Entry<'sc>,
    cp: OpAddr
}

impl<'sc, S: Storage> Scope<'sc, S> {
    pub fn new(code: S::Entry<'sc>, regs: Vec<S::Entry<'sc>>) -> Self {
        Scope {
            code, regs,
            cp: 0
        }
    }

    pub fn code(&self) -> &S::Entry<'sc> {
        &self.code
    }

    // We use an external store in
    // order to prevent immutably borrowing self so that
    // the scope can be mutated while holding a pointer to the op
    pub fn current<'c>(&self, code: &CodeReader<'c>) -> Result<OpReader<'c>, ExecError> {
        let or = code.get_ops()?.get(self.cp);
        Ok(or)
    }

    pub fn reg(&self, r: RegAddr) -> Option<&S::Entry<'sc>> {
        self.regs.get(r as usize)
    }
}

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
