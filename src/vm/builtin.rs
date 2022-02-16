use crate::value::{ObjHandle};
use super::ExecError;
use super::machine::{Machine};
use super::tracer::ExecCache;

pub fn is_sync(_builtin: &str) -> bool {
    true
}

pub async fn async_builtin<'s, 'e, S: Storage, E : ExecCache<'s, S>>(_mach: &Machine<'s, 'e, S, E>, 
                        name: &str, _args: Vec<S::ObjectRef<'s>>) -> Result<S::ObjectRef<'s>, ExecError> {
    match name {
        _ => return Err(ExecError::new("Unrecognized async builtin"))
    }
}

pub fn numeric_binary_builtin<'s, 'e, S: Storage, E : ExecCache<'s, S>, F: FnOnce(Numeric, Numeric) -> Numeric>(
                                mach: &Machine<'s, 'e, S, E>, mut args: Vec<S::ObjectRef<'s>>,
                                f: F) -> Result<S::ObjectRef<'s>, ExecError> {
    let right = args.pop().unwrap();
    let left= args.pop().unwrap();
    let l_data = left.value()?;
    let r_data = right.value()?;
    let l = l_data.reader().numeric()?;
    let r = r_data.reader().numeric()?;
    let res = f(l, r);
    let entry = mach.store.insert_build::<ExecError, _>(|f| {
        res.set(f.init_primitive());
        Ok(())
    })?;
    Ok(entry)
}

pub fn insert_builtin<'s, 'e, S: Storage, E : ExecCache<'s, S>>
                                (mach: &Machine<'s, 'e, S, E>, mut args: Vec<S::ObjectRef<'s>>)
                                -> Result<S::ObjectRef<'s>, ExecError> {
    let value = args.pop().unwrap();
    let key = args.pop().unwrap();
    let record = args.pop().unwrap();
    // key and record are forced!
    let mut rec = record.value()?.reader().record()?;
    rec.push((key.ptr(), value.ptr()));
    mach.store.insert_build::<ExecError, _>(|v| {
        let mut r = v.init_record(rec.len() as u32);
        for (i, (k, v)) in rec.into_iter().enumerate() {
            let mut e = r.reborrow().get(i as u32);
            e.set_key(k.raw());
            e.set_val(v.raw());
        }
        Ok(())
    })
}

pub fn sync_builtin<'s, 'e, S: Storage, E : ExecCache<'s, S>>(mach: &Machine<'s, 'e, S, E>, 
                        name: &str, args: Vec<S::ObjectRef<'s>>) -> Result<S::ObjectRef<'s>, ExecError> {
    match name {
        "add" => numeric_binary_builtin(mach, args, Numeric::add),
        "sub" => numeric_binary_builtin(mach, args, Numeric::sub),
        "mul" => numeric_binary_builtin(mach, args, Numeric::mul),
        "div" => numeric_binary_builtin(mach, args, Numeric::div),
        "insert" => insert_builtin(mach, args),
        _ => return Err(ExecError::new("Unrecognized builtin"))
    }
}