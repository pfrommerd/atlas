use crate::value::{Storage, ObjectRef, ValueRef, ExtractValue, Numeric};
use super::ExecError;
use super::machine::{Machine};
use super::tracer::ExecCache;

pub fn is_sync(_builtin: &str) -> bool {
    true
}

pub async fn async_builtin<'s, 'e, S: Storage, E : ExecCache<'s, S>>(_mach: &Machine<'s, 'e, S, E>, 
                        name: &str, _args: Vec<S::ObjectRef<'s>>) -> Result<S::ObjectRef<'s>, ExecError> {
    match name {
        _ => return Err(ExecError {})
    }
}

pub fn sync_builtin<'s, 'e, S: Storage, E : ExecCache<'s, S>>(mach: &Machine<'s, 'e, S, E>, 
                        name: &str, mut args: Vec<S::ObjectRef<'s>>) -> Result<S::ObjectRef<'s>, ExecError> {
    match name {
        "add" => {
            let right = args.pop().unwrap();
            let left= args.pop().unwrap();
            let l_data = left.value()?;
            let r_data = right.value()?;
            let l = l_data.reader().numeric()?;
            let r = r_data.reader().numeric()?;
            let res = Numeric::op(l, r, 
            |a, b| a + b, |a, b| a + b);

            let entry = mach.store.insert_build(|f| {
                res.set(f.init_primitive());
                Ok(())
            })?;
            Ok(entry)
        },
        _ => return Err(ExecError {})
    }
}