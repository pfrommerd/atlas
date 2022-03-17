use smol::LocalExecutor;

use crate::compile::Compile;
use crate::value::{Env, Storage, ObjHandle, OwnedValue, Numeric, CodeReader, op::BuiltinReader};
use crate::{Error, ErrorKind};
use super::machine::Machine;
use super::scope::{Registers, ExecQueue};
use super::tracer::ExecCache;

pub fn compile_op<'a, 'e, A, E>(mach: &Machine<'a, 'e, A, E>, mut args: Vec<ObjHandle<'a, A>>) 
                    -> Result<ObjHandle<'a, A>, Error> 
                where S: Storage, E: ExecCache<'a, A> {
    let source = args.pop().unwrap().as_str()?;

    let mut env = Env::new();
    crate::vm::populate_prelude(mach.alloc, &mut env)?;
    let lexer = crate::parse::Lexer::new(source.as_str());
    let parser = crate::grammar::ModuleParser::new();
    let module : crate::parse::ast::Module = parser.parse(lexer).unwrap();
    let expr = module.transpile();
    let compiled = expr.compile(mach.alloc, &Env::new())?;
    Ok(compiled)
}

pub async fn read_file_op<'a, 'e, A, E>(mach: &Machine<'a, 'e, A, E>, mut args: Vec<ObjHandle<'a, A>>) 
                    -> Result<ObjHandle<'a, A>, Error>
                where S: Storage, E: ExecCache<'a, A> {
    let path = args.pop().unwrap().as_str()?;
    let contents = std::fs::read_to_string(path)
        .map_err(|_| Error::new_const(ErrorKind::IO, "Couldn't read file"))?;
    OwnedValue::String(contents).pack_new(mach.alloc)
}

pub fn numeric_binary_op<'a, 'e, S: Storage, E : ExecCache<'a, A>, F: FnOnce(Numeric, Numeric) -> Numeric>(
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

pub fn insert_op<'a, 'e, A, E>(mach: &Machine<'a, 'e, A, E>, mut args: Vec<ObjHandle<'a, A>>)
                                -> Result<ObjHandle<'a, A>, Error> 
                where S: Storage, E: ExecCache<'a, A>{
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

pub fn project_op<'a, 'e, A, E>(_mach: &Machine<'a, 'e, A, E>, mut args: Vec<ObjHandle<'a, A>>)
                                -> Result<ObjHandle<'a, A>, Error> 
                where S: Storage, E: ExecCache<'a, A>{
    let key = args.pop().unwrap();
    let record = args.pop().unwrap();
    let key_str = key.as_str()?;
    let record = record.as_record()?;
    for (k, v) in record {
        if k.as_str()? == key_str {
            return Ok(v)
        }
    }
    Err(Error::new(format!("Could not project {key_str} into record")))
}

pub fn exec_builtin<'t, 'a, 'e, A, E>(mach: &'t Machine<'a, 'e, A, E>, op : BuiltinReader<'t>, code: CodeReader<'t>, thunk_ex: &LocalExecutor<'t>,
                regs: &'t Registers<'a, A>, queue: &'t ExecQueue) -> Result<(), Error> 
        where
            S: Storage,
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
            "project" => project_op(mach, args),
            "compile" => compile_op(mach, args),
            _ => { break; }
        }?;
        regs.set_object(op.get_dest()?, res)?;
        queue.complete(op.get_dest()?, code)?;
        return Ok(());
    }
    // Run the op asynchronously (this will then fail if the op is not recognized)
    thunk_ex.spawn(async move {
        let res = match name {
            "read_file" => read_file_op(mach, args).await,
            _ => panic!("Unknown op {name}")
        }.unwrap();
        regs.set_object(op.get_dest().unwrap(), res).unwrap();
        queue.complete(op.get_dest().unwrap(), code).unwrap();
    }).detach();
    Ok(())
}