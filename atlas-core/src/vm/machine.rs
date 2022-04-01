use crate::{Error, ErrorKind};
use crate::compile::{Compile, Env};
use crate::store::{Storage, ThunkMap, Storable, PartialReader, ObjectType, ObjectReader, CodeReader, 
                    RecordReader, TupleReader, Handle, ReaderWhich, Numeric, 
                    StringReader, BufferReader};
use crate::store::op::{Op, BuiltinOp};
use crate::store::value::Value;
use crate::store::print::Depth;

use super::resource::ResourceProvider;
use super::scope::{ExecQueue, Registers, ExecItem};
use super::scope;

use std::borrow::Borrow;
use std::ops::Deref;
use futures_lite::future::BoxedLocal;
use smol::LocalExecutor;
use pretty::{BoxAllocator, BoxDoc};
use bytes::Bytes;
use std::collections::HashMap;
use std::rc::Rc;
use url::Url;

pub trait SyscallHandler<'s, S : Storage + 's> {
    fn call(&self, mach: &Machine<'s, S>, args: Vec<S::Handle<'s>>)
        -> BoxedLocal<Result<S::Handle<'s>, Error>>;
}

pub struct Machine<'s, S: Storage> {
    store: &'s S, 
    thunk_map: Rc<S::ThunkMap<'s>>,
    resources: Rc<dyn ResourceProvider<'s, S> + 's>,
    syscalls: HashMap<String, Rc<dyn SyscallHandler<'s, S> + 's>>
}

impl<'s, S: Storage> Machine<'s, S> {
    pub fn new(store: &'s S, thunk_map: Rc<S::ThunkMap<'s>>, resources: Rc<dyn ResourceProvider<'s, S> + 's>) -> Self {
        Self { 
            store, thunk_map, resources,
            syscalls: HashMap::new()
        }
    }

    pub fn add_syscall<O: ToOwned<Owned=String>>(&mut self, sys: O, handler: Rc<dyn SyscallHandler<'s, S>>) {
        self.syscalls.insert(sys.to_owned(), handler);
    }

    pub async fn env_use(&self, mod_ref: S::Handle<'s>, env: &mut Env<S::Handle<'s>>) -> Result<(), Error> {
        let handle = self.force(&mod_ref).await?;
        let reader = handle.reader()?;
        let rec = reader.as_record()?;
        for (k, v) in rec.iter() {
            let key_reader = k.borrow().reader()?;
            let key_str = key_reader.as_string()?;
            let value = v.borrow().clone();
            env.insert(key_str.as_slice().to_owned(), value);
        }
        Ok(())
    }

    // Does the actual forcing in a loop, and checks the trace cache first
    pub async fn force(&self, thunk_ref: &S::Handle<'s>)
            -> Result<S::Handle<'s>, Error> {
        let mut thunk_ref = thunk_ref.clone();
        loop {
            log::trace!(target: "vm", "trying {}", thunk_ref);
            // first check the cache for this thunk
            let _type = thunk_ref.reader()?.get_type();
            if ObjectType::Thunk != _type {
                log::trace!(target: "vm", "{} is already WHNF of type {:?}", thunk_ref, _type);
                return Ok(thunk_ref)
            }
            // check the cache for this particular thunk
            let next_thunk = if let Some(v) = self.thunk_map.get(&thunk_ref) {
                v
            } else {
                log::trace!(target: "vm", "forcing {}", thunk_ref);
                let res = self.force_stack(thunk_ref.clone()).await?;
                self.thunk_map.insert(&thunk_ref, &res);
                res
            };
            thunk_ref = next_thunk;
            if ObjectType::Thunk != thunk_ref.reader()?.get_type() {
                return Ok(thunk_ref)
            }
        }
    }

    // Does a single stack worth of forcing (and returns)
    async fn force_stack(&self, thunk_ref: S::Handle<'s>) -> Result<S::Handle<'s>, Error> {
        // get the entry ref 
        let entry_ref = thunk_ref.reader()?.as_thunk()?;
        let (code_ref, inputs) = match entry_ref.borrow().reader()?.which() {
            ReaderWhich::Code(_) => (entry_ref.borrow().clone(), Vec::new()),
            ReaderWhich::Partial(p) => {
                (p.get_code().borrow().clone(), p.iter_args().map(|x| x.borrow().clone()).collect())
            },
            _ => return Err(Error::new_const(ErrorKind::Internal, "Force target is not code or a partial"))
        };
        let code_reader = code_ref.reader()?.as_code()?;
        let queue = ExecQueue::new();
        let regs = Registers::new(self.store, code_reader.get_ret());

        {
            let code_doc: BoxDoc<'_, ()> = code_reader.pretty(Depth::Fixed(2), &BoxAllocator).into_doc();
            log::trace!(target: "vm", "thunk {} executing:\n{}", thunk_ref, code_doc.pretty(80));
        }

        for op_addr in code_reader.iter_ready() {
            queue.push(op_addr);
        }

        log::trace!(target: "vm", "populated thunk {}", thunk_ref);

        // We need to drop the local executor before the queue, regs
        let thunk_ex = LocalExecutor::new();
        let res : Result<S::Handle<'s>, Error> = thunk_ex.run(async {
            loop {
                match queue.next_op().await {
                ExecItem::Op(addr) => {
                    let op = code_reader.get_op(addr);
                    log::trace!(target: "vm", "executing #{} for thunk {} (code {}): {}", addr, thunk_ref, code_ref, op);
                    self.exec_op(op, &code_reader, &thunk_ex, &regs, &queue, &inputs)?;
                },
                ExecItem::Ret(h) => return Ok(h),
                ExecItem::Err(e) => return Err(e)
                }
            }
        }).await;
        match &res {
            Err(e) => log::trace!(target: "vm", "thunk {} return error {:?}", thunk_ref, e),
            _ => ()
        }
        log::trace!(target: "vm", "done with thunk {}", thunk_ref);
        res
    }

    fn exec_op<'t, 'r, R: CodeReader<'r, 's, Handle=S::Handle<'s>>>(&'t self, op : Op, code: &'t R, thunk_ex: &LocalExecutor<'t>,
                    regs: &'t Registers<'s, S>, queue: &'t ExecQueue<'s, S>, inputs: &Vec<S::Handle<'s>>) -> Result<(), Error> {
        use Op::*;
        match op {
            Force(dest, arg) => {
                let entry = regs.consume(arg)?;
                if entry.reader()?.get_type() == ObjectType::Thunk {
                    // spawn the force as a background task
                    // since we might want to move onto other things
                    thunk_ex.spawn(async move {
                        let res = self.force(&entry).await;
                        // we need to get 
                        scope::complete(code, regs, queue, &dest, res)
                    }).detach();
                } else {
                    // We are already WHNF
                    scope::complete(code, regs, queue, &dest, Ok(entry))
                }
            },
            SetValue(dest, value) => {
                let h = code.get_value(value).unwrap();
                scope::complete(code, regs, queue, &dest, Ok(h.borrow().clone()))
            },
            SetInput(dest, input) => {
                let val = inputs.get(input as usize).cloned()
                        .ok_or(Error::new_const(ErrorKind::Internal, "Input out of bounds"));
                scope::complete(code, regs, queue, &dest, val)
            },
            Bind(dest, lam, bind_args) => {
                let lam = regs.consume(lam)?;
                let (code_entry, mut args) = match lam.reader()?.get_type() {
                    ObjectType::Code => (lam.clone(), Vec::new()),
                    ObjectType::Partial => {
                        let partial = lam.reader()?.as_partial()?;
                        let code = partial.get_code().borrow().clone();
                        let args = partial.iter_args().map(|x| x.borrow().clone()).collect();
                        (code, args)
                    },
                    _ => return Err(Error::new_const(ErrorKind::Internal, "Can only bind to a code or partial"))
                };
                let new_args : Result<Vec<S::Handle<'s>>, Error> = 
                    bind_args.iter().map(|x| regs.consume(*x)).collect();
                args.extend(new_args?);
                // construct a new partial with the modified arguments
                let res = self.store.insert_from(&Value::Partial(code_entry, args))?;
                scope::complete(code, regs, queue, &dest, Ok(res))
            },
            Invoke(dest, target) => {
                let target_entry = regs.consume(target)?;
                let entry = self.store.insert_from(&Value::Thunk(target_entry))?;
                scope::complete(code, regs, queue, &dest, Ok(entry))
            },
            Builtin(dest, op, args) => {
                // builtin::exec_builtin(self, r, code, thunk_ex, regs, queue)?;
                let args : Result<Vec<S::Handle<'s>>, Error> = args.iter().map(|&x| regs.consume(x)).collect();
                let mut args = args?;
                use BuiltinOp::*;
                let res = match op {
                    Add | Sub | Mul | Div => {
                        let rhs = args.pop().unwrap();
                        let lhs = args.pop().unwrap();
                        if !args.is_empty() { panic!("Expected two arguments") }
                        match op {
                            Add => self.numeric_binop(rhs, lhs, Numeric::add),
                            Sub => self.numeric_binop(rhs, lhs, Numeric::sub),
                            Mul => self.numeric_binop(rhs, lhs, Numeric::mul),
                            Div => self.numeric_binop(rhs, lhs, Numeric::div),
                            _ => panic!("Unexpected")
                        }
                    },
                    Neg => {
                        let arg = args.pop().unwrap();
                        if !args.is_empty() { panic!("Expected one argument") }
                        self.numeric_unop(arg, Numeric::neg)
                    },
                    EmptyRecord => self.store.insert_from(&Value::Record(Vec::new())),
                    EmptyTuple => self.store.insert_from(&Value::Tuple(Vec::new())),
                    Append => {
                        let item = args.pop().unwrap();
                        let object = args.pop().unwrap();
                        self.append(object, item)
                    },
                    Insert => {
                        let value = args.pop().unwrap();
                        let key = args.pop().unwrap();
                        let object = args.pop().unwrap();
                        self.insert(object, key, value)
                    },
                    Project => {
                        let key = args.pop().unwrap();
                        let object = args.pop().unwrap();
                        self.project(object, key)
                    },
                    Nil => self.store.insert_from(&Value::Nil),
                    Cons => {
                        let tail = args.pop().unwrap();
                        let head = args.pop().unwrap();
                        self.store.insert_from(&Value::Cons(head, tail))
                    },
                    JoinUrl => {
                        let ext = args.pop().unwrap();
                        let base = args.pop().unwrap();
                        let ext_str : _ = ext.reader()?.as_string()?;
                        let base_str : _ = base.reader()?.as_string()?;
                        let ext_str : _ = ext_str.as_slice();
                        let base_str : _ = base_str.as_slice();
                        let base_url = 
                            Url::parse(base_str.deref())
                            .map_err(|_| Error::new("Malformed url"))?;
                        let joined_url = base_url.join(ext_str.deref())
                            .map_err(|_| Error::new("Malformed url"))?;
                        self.store.insert_from(&Value::String(joined_url.to_string()))
                    },
                    DecodeUtf8 => {
                        let bytes = args.pop().unwrap();
                        let buff : _ = bytes.reader()?.as_buffer()?;
                        let buff = buff.as_slice();
                        let str = std::str::from_utf8(buff.deref())
                                .map_err(|_| Error::new("Invalid utf8"))?;
                        self.store.insert_from(&Value::String(String::from(str)))
                    },
                    EncodeUtf8 => {
                        let str = args.pop().unwrap();
                        let str : _ = str.reader()?.as_string()?;
                        let str = str.as_slice();
                        self.store.insert_from(&Value::Buffer(Bytes::copy_from_slice(str.deref().as_bytes())))

                    },
                    // Fetch, Compile, and Sys are the only async builtins!
                    Fetch => {
                        let url = args.pop().unwrap();
                        thunk_ex.spawn(async move {
                            let res : Result<S::Handle<'s>, Error> = try {
                                let url_str : _ = url.reader()?.as_string()?;
                                let url_str : _ = url_str.as_slice();
                                let url = Url::parse(url_str.deref())
                                    .map_err(|_| Error::new("Bad url"))?;
                                self.fetch(&url).await?
                            };
                            scope::complete(code, regs, queue, &dest, res)
                        }).detach();
                        return Ok(());
                    },
                    Compile => {
                        let text = args.pop().unwrap();
                        let loc = args.pop().unwrap();
                        thunk_ex.spawn(async move {
                            let res : Result<S::Handle<'s>, Error> = try {
                                let text_str : _ = text.reader()?.as_string()?;
                                let text_str : _ = text_str.as_slice();
                                self.compile_module(loc, text_str.deref()).await?
                            };
                            scope::complete(code, regs, queue, &dest, res)
                        }).detach();
                        return Ok(());
                    },
                    Sys => {
                        thunk_ex.spawn(async move {
                            let res = try {
                                let sys_args = args.split_off(1);
                                let sys_name = args.pop().unwrap();
                                let sys_str : _ = sys_name.reader()?.as_string()?;
                                let sys_str : _ = sys_str.as_slice();
                                self.sys(sys_str.deref(), sys_args).await?
                            };
                            scope::complete(code, regs, queue, &dest, res)
                        }).detach();
                        return Ok(());
                    }
                };
                scope::complete(code, regs, queue, &dest, res)
            },
            Match(_dest, _scrut, _cases) => {
                todo!()
                // // get the value we are supposed to be matching
                // let scrut = regs.consume(r.get_scrut())?;
                // // get the case of the value
                // let opt = self.compute_match(&scrut, r.reborrow())?;
                // regs.set_object(r.get_dest()?, opt)?;
                // queue.complete(r.get_dest()?, code.reborrow())?;
            },
        }
        Ok(())
    }

    pub fn numeric_binop<F: Fn(Numeric, Numeric) -> Numeric>(&self, lhs: S::Handle<'s>, rhs: S::Handle<'s>, func : F) -> Result<S::Handle<'s>, Error> {
        let (l, r) = (lhs.reader()?.as_numeric()?, rhs.reader()?.as_numeric()?);
        self.store.insert_from(&Value::from_numeric(func(l, r)))
    }

    pub fn numeric_unop<F: Fn(Numeric) -> Numeric>(&self, arg: S::Handle<'s>, func : F) -> Result<S::Handle<'s>, Error> {
        let arg = arg.reader()?.as_numeric()?;
        self.store.insert_from(&Value::from_numeric(func(arg)))
    }

    // Assumes the object is forced!
    pub fn insert(&self, obj: S::Handle<'s>, key: S::Handle<'s>, val: S::Handle<'s>) -> Result<S::Handle<'s>, Error> {
        use ReaderWhich::*;
        match obj.reader()?.which() {
            Record(r) => {
                let mut entries = Vec::new();
                {
                    let insert_str = key.reader()?.as_string()?;
                    let insert_str = insert_str.as_slice();
                    for (k,v) in r.iter() {
                        let key_str = k.borrow().reader()?.as_string()?;
                        let key_str = key_str.as_slice();
                        if key_str.deref() != insert_str.deref() {
                            entries.push((k.borrow().clone(), v.borrow().clone()))
                        }
                    }
                }
                entries.push((key, val));
                self.store.insert_from(&Value::Record(entries))
            },
            _ => Err(Error::new("Expected record"))
        }
    }

    // Assumes the object is forced!
    pub fn project(&self, obj: S::Handle<'s>, key: S::Handle<'s>) -> Result<S::Handle<'s>, Error> {
        use ReaderWhich::*;
        match obj.reader()?.which() {
            Record(r) => {
                let key_str = key.reader()?.as_string()?;
                let key_str = key_str.as_slice();
                Ok(r.get(key_str.deref())?.borrow().clone())
            },
            _ => Err(Error::new_const(ErrorKind::BadType, "Bad type, not a record"))
        }
    }

    // Assumes the object is forced!
    pub fn append(&self, obj: S::Handle<'s>, item: S::Handle<'s>) -> Result<S::Handle<'s>, Error> {
        use ReaderWhich::*;
        match obj.reader()?.which() {
            Tuple(t) => {
                let mut items : Vec<_> =  t.iter().map(|x| x.borrow().clone()).collect();
                items.push(item);
                self.store.insert_from(&Value::Tuple(items))
            },
            _ => Err(Error::new_const(ErrorKind::BadType, "Bad type, not a tuple"))
        }
    }

    pub async fn compile_module(&self, loc: S::Handle<'s>, source: &str) -> Result<S::Handle<'s>, Error> {
        // Get the prelude from the resources
        let prelude_src = self.fetch(&Url::parse("builtin://prelude").unwrap()).await?;
        let prelude_src = prelude_src.reader()?.as_string()?;
        let prelude_src = prelude_src.as_slice();
        let prelude_src = prelude_src.deref();

        let mut env = Env::new();
        env.insert(String::from("__path__"), loc);
        // Load the prelude
        {
            let lexer = crate::parse::Lexer::new(prelude_src);
            let parser = crate::grammar::ModuleParser::new();
            let module : crate::parse::ast::Module = parser.parse(lexer).unwrap();
            let expr = module.transpile();
            let prelude_compiled = expr.compile(self.store, &env)?.store_in(self.store)?;
            let prelude_module = self.store.insert_from(&Value::Thunk(prelude_compiled))?;
            self.env_use(prelude_module, &mut env).await?;
        }

        let lexer = crate::parse::Lexer::new(source);
        let parser = crate::grammar::ModuleParser::new();
        let module : crate::parse::ast::Module = parser.parse(lexer).unwrap();
        let expr = module.transpile();
        let code = expr.compile(self.store, &env)?.store_in(self.store)?;
        self.store.insert_from(&Value::Thunk(code))
    }

    // Do a syscall
    pub async fn sys(&self, sys: &str, args: Vec<S::Handle<'s>>) -> Result<S::Handle<'s>, Error> {
        let handler = self.syscalls.get(sys).ok_or(Error::new_const(ErrorKind::NotFound, "Syscall not found"))?;
        handler.call(self, args).await
    }

    pub async fn fetch(&self, url: &Url) -> Result<S::Handle<'s>, Error> {
        self.resources.retrieve(url).await
    }
}