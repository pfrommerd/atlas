use smol::LocalExecutor;

use crate::value::{Allocator, ObjHandle, OwnedValue, Numeric, CodeReader, op::BuiltinReader};
use crate::Error;
use super::machine::Machine;
use super::scope::{Registers, ExecQueue};
use super::tracer::ExecCache;


pub fn numeric_binary_op<'a, 'e, A: Allocator, E : ExecCache<'a, A>, F: FnOnce(Numeric, Numeric) -> Numeric>(
                                mach: &Machine<'a, 'e, A, E>, mut args: Vec<ObjHandle<'a, A>>,
                                f: F) -> Result<ObjHandle<'a, A>, Error> {
    let right = args.pop().unwrap();
    let left= args.pop().unwrap();
    let l_data = left.as_numeric()?;
    let r_data = right.as_numeric()?;
    let res = f(l_data, r_data);
    let entry = OwnedValue::Numeric(res).pack_new(mach.alloc)?;
    Ok(entry)
}

pub fn insert_op<'a, 'e, A: Allocator, E : ExecCache<'a, A>>
                                (mach: &Machine<'a, 'e, A, E>, mut args: Vec<ObjHandle<'a, A>>)
                                -> Result<ObjHandle<'a, A>, Error> {
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

pub fn exec_builtin<'t, 'a, 'e, A, E>(mach: &'t Machine<'a, 'e, A, E>, op : BuiltinReader<'t>, code: CodeReader<'t>, thunk_ex: &LocalExecutor<'t>,
                regs: &'t Registers<'a, A>, queue: &'t ExecQueue) -> Result<(), Error> 
        where
            A: Allocator,
            E: ExecCache<'a, A> {
    let name = op.get_op()?;
    // consume the arguments
    let args : Result<Vec<ObjHandle<'a, A>>, Error> = 
        op.get_args()?.into_iter().map(|x| regs.consume(x)).collect();
    let args = args?;

    // Check the synchronous ops
    loop {
        let res = match name {
            "add" => numeric_binary_op(mach, args, Numeric::add),
            "sub" => numeric_binary_op(mach, args, Numeric::sub),
            "mul" => numeric_binary_op(mach, args, Numeric::mul),
            "div" => numeric_binary_op(mach, args, Numeric::div),
            "empty_record" => OwnedValue::Record(vec![]).pack_new(mach.alloc),
            "empty_tuple" => OwnedValue::Tuple(vec![]).pack_new(mach.alloc),
            "empty_list" => OwnedValue::Nil.pack_new(mach.alloc),
            "insert" => insert_op(mach, args),
            _ => { break; }
        }?;
        regs.set_object(op.get_dest()?, res)?;
        queue.complete(op.get_dest()?, code)?;
        return Ok(());
    }
    // Run the op asynchronously (this will then fail if the op is not recognized)
    thunk_ex.spawn(async move {
        let res = match name {
            "read_file" => numeric_binary_op(mach, args, Numeric::add),
            _ => panic!("Unknown op {name}")
        }.unwrap();
        regs.set_object(op.get_dest().unwrap(), res).unwrap();
        queue.complete(op.get_dest().unwrap(), code).unwrap();
    }).detach();
    Ok(())
}