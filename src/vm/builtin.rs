use crate::value::{Allocator, ObjHandle, OwnedValue, Numeric};
use super::ExecError;
use super::machine::{Machine};
use super::tracer::ExecCache;

pub fn is_sync(_builtin: &str) -> bool {
    true
}

pub async fn async_builtin<'a, 'e, A: Allocator, E : ExecCache<'a, A>>(_mach: &Machine<'a, 'e, A, E>, 
                        name: &str, _args: Vec<ObjHandle<'a, A>>) -> Result<ObjHandle<'a, A>, ExecError> {
    match name {
        _ => return Err(ExecError::new("Unrecognized async builtin"))
    }
}

pub fn numeric_binary_builtin<'a, 'e, A: Allocator, E : ExecCache<'a, A>, F: FnOnce(Numeric, Numeric) -> Numeric>(
                                mach: &Machine<'a, 'e, A, E>, mut args: Vec<ObjHandle<'a, A>>,
                                f: F) -> Result<ObjHandle<'a, A>, ExecError> {
    let right = args.pop().unwrap();
    let left= args.pop().unwrap();
    let l_data = left.as_numeric()?;
    let r_data = right.as_numeric()?;
    let res = f(l_data, r_data);
    let entry = OwnedValue::Numeric(res).pack_new(mach.alloc)?;
    Ok(entry)
}

pub fn insert_builtin<'a, 'e, A: Allocator, E : ExecCache<'a, A>>
                                (mach: &Machine<'a, 'e, A, E>, mut args: Vec<ObjHandle<'a, A>>)
                                -> Result<ObjHandle<'a, A>, ExecError> {
    let value = args.pop().unwrap();
    let key = args.pop().unwrap();
    let key_str = key.as_str()?;
    let record = args.pop().unwrap();
    let mut record = record.as_record()?;

    let mut append = true;
    for (k, v) in record.iter_mut() {
        if k.as_str()? == key_str {
            *v = value.clone();
            append = false;
        }
    }
    if append {
        record.push((key, value));
    }
    Ok(OwnedValue::Record(record).pack_new(mach.alloc)?)
}

pub fn sync_builtin<'a, 'e, A: Allocator, E : ExecCache<'a, A>>(mach: &Machine<'a, 'e, A, E>, 
                        name: &str, args: Vec<ObjHandle<'a, A>>) -> Result<ObjHandle<'a, A>, ExecError> {
    match name {
        "add" => numeric_binary_builtin(mach, args, Numeric::add),
        "sub" => numeric_binary_builtin(mach, args, Numeric::sub),
        "mul" => numeric_binary_builtin(mach, args, Numeric::mul),
        "div" => numeric_binary_builtin(mach, args, Numeric::div),
        "insert" => insert_builtin(mach, args),
        _ => return Err(ExecError::new("Unrecognized builtin"))
    }
}