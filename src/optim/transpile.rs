use super::graph::{
    LamGraph, OpNode, CompRef
};
use super::compile::{Compile, CompileError};
use crate::core::lang::{
    DisambID, ExprReader, ExprWhich, ApplyWhich,
    PrimitiveReader, PrimitiveWhich, ParamWhich, Symbol
};
use crate::value::{Storage, ObjectRef};
use super::Env;
use std::collections::{HashMap, HashSet};

#[derive(Debug)]
pub struct TranspileError {}

impl From<capnp::Error> for TranspileError {
    fn from(_: capnp::Error) -> Self {
        Self {}
    }
}
impl From<capnp::NotInSchema> for TranspileError {
    fn from(_: capnp::NotInSchema) -> Self {
        Self {}
    }
}
impl From<CompileError> for TranspileError {
    fn from(_: CompileError) -> Self {
        Self {}
    }
}

pub struct TranspileContext<'e, 'g, 'l, 's, S: Storage> {
    pub locals : &'g TranspileEnv<'l>,
    pub graph: &'g LamGraph<'e, 's, S>,
    pub store: &'s S
}

pub trait Transpile<'e> {
    // This should transpile to a WNHF-forced representation
    fn transpile_with<S: Storage>(&self, ctx: &TranspileContext<'e, '_, '_, '_, S>) 
            -> Result<CompRef, TranspileError>;

    // Transpile is a top-level callable. It takes a store and a map of bound variables in the store
    // and will transpile the expression to a thunk with the given env bound
    fn transpile<'s, S: Storage>(&self, store: &'s S, env: &Env<'s, S>) 
                        -> Result<S::ObjectRef<'s>, TranspileError> {
        let mut graph = LamGraph::default();
        let mut locals = TranspileEnv::new();
        let mut vals = Vec::new();
        env.map.iter().for_each(|(s, v)| {
            let inp = graph.create_input();
            vals.push(v);
            locals.add((s.as_str(), 0), inp);
        });
        let res = self.transpile_with(&TranspileContext {
            locals: &locals,
            graph: &graph,
            store
        })?;
        let res_force = graph.ops.insert(OpNode::Force(res));
        graph.create_ret(res_force);
        let lam = graph.compile(store)?;
        let partial = store.insert_build::<TranspileError, _>(|b| {
            let mut part = b.init_partial();
            part.set_code(lam.ptr().raw());
            let mut args = part.init_args(vals.len() as u32);
            for (i, v) in vals.iter().enumerate() {
                args.set(i as u32, v.ptr().raw())
            }
            Ok(())
        })?;
        // construct a thunk from the partial
        Ok(store.insert_build::<TranspileError, _>(|mut b| {
            b.set_thunk(partial.ptr().raw()); Ok(())
        })?)
    }
}

impl<'e> Transpile<'e> for PrimitiveReader<'e> {
    fn transpile_with<S: Storage>(&self, ctx: &TranspileContext<'e, '_, '_, '_, S>) 
            -> Result<CompRef, TranspileError> {
        // build a value in the storage
        use PrimitiveWhich::*;
        let p = ctx.store.insert_build::<TranspileError,_>(|b| {
            let mut p = b.init_primitive();
            match self.which()? {
                Unit(_) => p.set_unit(()),
                Bool(b) => p.set_bool(b),
                Int(i) => p.set_int(i),
                Float(f) => p.set_float(f),
                Char(c) => p.set_char(std::char::from_u32(c).ok_or(TranspileError {})? as u32),
                String(s) => p.set_string(s?),
                Buffer(b) => p.set_buffer(b?),
                EmptyList(_) => p.set_empty_list(()),
                EmptyTuple(_) => p.set_empty_tuple(()),
                EmptyRecord(_) => p.set_empty_record(())
            }
            Ok(())
        })?;
        Ok(ctx.graph.ops.insert(OpNode::External(p)))
    }
}

// This is used both for transpiling lambdas, as well
// as transpiling arguments (which for laziness purposes need to be treated as 0-argument lambdas)
// and let-bound variables (which need to be lambdas/thunks in order to ensure value reuse)
fn transpile_lambda_body<'e, S: Storage>(body: &ExprReader<'e>, 
                            parent_ctx: &TranspileContext<'e, '_, '_, '_, S>, params: Vec<Symbol<'e>>) 
                            -> Result<CompRef, TranspileError> {
    let bound_syms : HashSet<_> = params.iter().cloned().collect();
    let fv = body.free_variables(&bound_syms);

    // The graph for the lambda body
    let mut new_graph = LamGraph::default();
    // a new symbol environment
    let mut new_locals = TranspileEnv::new();
    let mut args = Vec::new();

    // set up all of the free variables
    for sym in fv.into_iter() {
        // lift the symbol
        let inp_ptr = new_graph.create_input();
        new_locals.add(sym, inp_ptr);
        // push the symbol as an argument
        let ptr = parent_ctx.locals.get(sym).ok_or(TranspileError {})?;
        args.push(ptr)
    }
    for sym in params.into_iter() {
        // add an input (no lift parameters)
        let ptr = new_graph.create_input();
        new_locals.add(sym, ptr);
    }

    let res = body.transpile_with(
    &TranspileContext {
            locals: &new_locals,
            graph: &new_graph,
            store: parent_ctx.store
        }
    )?;
    let res_force = new_graph.ops.insert(OpNode::Force(res));
    // Set the output
    new_graph.create_ret(res_force);
    let entry = new_graph.compile(parent_ctx.store)?;
    let func_node = parent_ctx.graph.ops.insert(OpNode::External(entry));
    if args.len() == 0 {
        Ok(func_node)
    } else {
        Ok(parent_ctx.graph.ops.insert(OpNode::Bind(func_node, args)))
    }
}

fn transpile_lambda<'e, S: Storage>(expr: &ExprReader<'e>, ctx: &TranspileContext<'e, '_, '_, '_, S>)
                            -> Result<CompRef, TranspileError> {
    let lam = match expr.which().unwrap() {
        ExprWhich::Lam(l) => l,
        _ => panic!("Must supply lambda")
    };
    // set up all of the actual inputs
    let params = lam.get_params()?;
    let mut param_syms = Vec::new();
    for p in params.iter() {
        let s = p.get_symbol()?;
        let sym = (s.get_name()?, s.get_disam());
        match p.which()? {
            ParamWhich::Pos(()) => (),
            ParamWhich::Named(_) => (),
            _ => panic!("Unsupported param type")
        }
        param_syms.push(sym);
    }
    // compile body into graph
    transpile_lambda_body(&lam.get_body()?, ctx, param_syms)
}

fn transpile_apply<'e, S: Storage>(expr: &ExprReader<'e>, ctx: &TranspileContext<'e, '_,'_, '_, S>)
                            -> Result<CompRef, TranspileError> {
    let apply= match expr.which().unwrap() {
        ExprWhich::App(app) => app,
        _ => panic!("Must supply apply")
    };
    let mut args = Vec::new();
    let e = apply.get_lam()?.transpile_with(ctx)?;
    for a in apply.get_args()?.iter() {
        use ApplyWhich::*;
        // each argument needs to be transpiled into its
        // own lambda to ensure laziness and reuse
        let arg = a.get_value()?.transpile_with(ctx)?;
        match a.which()? {
            Pos(_) => (),
            _ => panic!("Unsupported argument type")
        };
        args.push(arg);
    }
    let apply_node = ctx.graph.ops.insert(OpNode::Bind(e, args));
    Ok(apply_node)
}

fn transpile_invoke<'e, S: Storage>(expr: &ExprReader<'e>, ctx: &TranspileContext<'e, '_, '_, '_, S>)
                            -> Result<CompRef, TranspileError> {
    let i = match expr.which().unwrap() {
        ExprWhich::Invoke(i) => i?,
        _ => panic!("Must supply apply")
    };
    let lam = i.transpile_with(ctx)?;
    let inv = ctx.graph.ops.insert(OpNode::Invoke(lam));
    // An invocation is not WHNF and so must be followed by a force
    // to actually calculate the value of the invocation
    let force = ctx.graph.ops.insert(
        OpNode::Force(inv));
    Ok(force)
}

fn transpile_let<'e, S: Storage>(expr: &ExprReader<'e>, ctx: &TranspileContext<'e, '_, '_, '_, S>)
                            -> Result<CompRef, TranspileError> {
    let l = match expr.which().unwrap() {
        ExprWhich::Let(l) => l,
        _ => panic!("Must supply apply")
    };
    let binds = l.get_binds()?.get_binds()?;
    let new_locals = if l.get_binds()?.get_rec() {
        panic!("Can't handle recursive graphs (yet)");
        /*
        let mut new_locals= ctx.locals.clone();
        let mut slots = Vec::new();
        for b in binds {
            let s = b.get_symbol()?;
            let sym = (s.get_name()?, s.get_disam());
            let slot = ctx.graph.ops.slot();
            new_locals.add(sym,slot.get_ref());
            slots.push(slot);
        }
        for b in binds {
            let ptr = transpile_lambda_body(&b.get_value()?, 
                    &TranspileContext {
                        locals: &new_locals,
                        graph: ctx.graph,
                        store: ctx.store
                    }, Vec::new())?;
        }
        new_locals
        */
    } else {
        let mut new_locals= ctx.locals.clone();
        for b in binds {
            let s = b.get_symbol()?;
            let sym = (s.get_name()?, s.get_disam());
            // transpile into a lambda with no arguments
            // so that the computed WHNF value is shared among all uses
            let ptr = transpile_lambda_body(&b.get_value()?, 
                    &TranspileContext {
                        locals: &new_locals,
                        graph: ctx.graph,
                        store: ctx.store
                    }, Vec::new())?;
            new_locals.add(sym, ptr);
        }
        new_locals
    };
    // transpile the body
    l.get_body()?.transpile_with(&TranspileContext {
        locals: &new_locals,
        graph: ctx.graph,
        store: ctx.store
    })
}


fn transpile_builtin<'e, S: Storage>(expr: &ExprReader<'e>, ctx: &TranspileContext<'e, '_, '_, '_, S>)
                            -> Result<CompRef, TranspileError> {
    let b = match expr.which().unwrap() {
        ExprWhich::InlineBuiltin(b) => b,
        _ => panic!("Must supply apply")
    };
    let s = b.get_op()?;
    if s == "force" {
        let arg = b.get_args()?.iter().nth(0).ok_or(TranspileError {})?;
        let a = arg.transpile_with(&ctx)?;
        Ok(ctx.graph.ops.insert(OpNode::Force(a)))
    } else {
        let mut args = Vec::new();
        for a in b.get_args()?.iter() {
            // These arguments should be compiled to WHNF directly
            // and not lambdified
            args.push(a.transpile_with(ctx)?);
        }
        let builtin_node  = ctx.graph.ops.insert(OpNode::Builtin(s, args));
        Ok(builtin_node)
    }
}

impl<'e> Transpile<'e> for ExprReader<'e> {
    fn transpile_with<S: Storage>(&self, ctx: &TranspileContext<'e, '_, '_, '_, S>)
                    -> Result<CompRef, TranspileError> {
        use ExprWhich::*;
        let t = self.which();
        match t? {
            Id(s) => {
                let sym = s?;
                let s = (sym.get_name()?, sym.get_disam());
                Ok(ctx.locals.get(s).ok_or(TranspileError {})?)
            },
            Literal(p) => p?.transpile_with(ctx),
            Let(_) => transpile_let(self, ctx),
            Lam(_) => transpile_lambda(self, ctx),
            App(_) => transpile_apply(self, ctx),
            Invoke(_) => transpile_invoke(self, ctx),
            InlineBuiltin(_) => transpile_builtin(self, ctx),
            Match(_) => panic!("Match not yet implemented"),
            Error(_) => panic!("Error() not expected")
        }
    }
}

pub struct TranspileEnv<'e> {
    symbols: HashMap<(&'e str, DisambID), CompRef>
}

impl<'e> Clone for TranspileEnv<'e> {
    fn clone(&self) -> Self {
        Self { symbols: self.symbols.clone() }
    }
}


impl<'e> TranspileEnv<'e> {
    pub fn new() -> Self {
        Self { symbols: HashMap::new() }
    }
    pub fn get(&self, sym: (&'e str, DisambID)) -> Option<CompRef> {
        let s = self.symbols.get(&sym)?;
        Some(*s)
    }
    pub fn add(&mut self, sym: (&'e str, DisambID), val: CompRef) {
        self.symbols.insert(sym, val);
    }
}