use crate::value::{Storage, ObjectRef, DataRef, ExtractValue, Numeric};
use super::ExecError;
use super::machine::{Machine};

pub fn is_sync(_builtin: &str) -> bool {
    true
}

pub async fn async_builtin<'s, 'e, S: Storage>(_mach: &Machine<'s, 'e, S>, 
                        name: &str, _args: Vec<S::EntryRef<'s>>) -> Result<S::EntryRef<'s>, ExecError> {
    match name {
        _ => return Err(ExecError {})
    }
}

pub fn sync_builtin<'s, 'e, S: Storage>(mach: &Machine<'s, 'e, S>, 
                        name: &str, mut args: Vec<S::EntryRef<'s>>) -> Result<S::EntryRef<'s>, ExecError> {
    match name {
        "add" => {
            let right = args.pop().unwrap();
            let left= args.pop().unwrap();
            let l_data = left.get_value()?;
            let r_data = right.get_value()?;
            let l = l_data.reader().numeric()?;
            let r = r_data.reader().numeric()?;
            let res = Numeric::op(l, r, 
            |a, b| a + b, |a, b| a + b);

            mach.store.insert_build(|f| {
                res.set(f.init_primitive());
                Ok(())
            })
        },
        _ => return Err(ExecError {})
    }
}