use super::graph::{
    CodeGraph, OpNode, CompRef
};
use crate::core::FreeVariables;
use crate::value::{Allocator, ObjHandle, owned::OwnedValue};
use super::{Env, CompileError};
use std::collections::{HashMap, HashSet};

impl From<capnp::Error> for CompileError {
    fn from(_: capnp::Error) -> Self {
        Self {}
    }
}
impl From<capnp::NotInSchema> for CompileError {
    fn from(_: capnp::NotInSchema) -> Self {
        Self {}
    }
}

pub trait Compile : FreeVariables {
    // This should transpile to a WNHF-forced representation
    fn compile_into<A: Allocator>(&self, alloc: &A, env: &CompileEnv<'_>, 
                                graph: &mut CodeGraph<'_, A>)
            -> Result<CompRef, CompileError>;

    // Transpile is a top-level callable. It takes a store and a map of bound variables in the store
    // and will transpile the expression to a thunk with the given env bound
    // and the compref returned by compile_into getting returned
    fn compile<'a, A: Allocator>(&self, alloc: &'a A, env: &Env<'_, A>) 
                        -> Result<ObjHandle<'a, A>, CompileError> {
        let mut graph = CodeGraph::default();
        let mut cenv= CompileEnv::new();
        let args = Vec::new();

        let mut free = self.free_variables(HashSet::new());
        for (s, v) in env.iter() {
            if free.contains(s.as_ref()) {
                let inp = graph.create_input();
                cenv.add((s.as_str(), 0), inp);
                free.remove(&s);
            }
        }
        // If there are still free variables, throw an error
        if !free.is_empty() { return Err(CompileError {}) }
        // Compile into the graph we created
        let res = self.compile_into(alloc, &cenv, &mut graph)?;
        // Force + return the result
        let res_force = graph.ops.insert(OpNode::Force(res));
        graph.create_ret(res_force);

        // Pack the graph
        let mut lam = graph.pack_new(alloc)?;
        // If there are free variables, bind them
        // as arguments
        if args.len() > 0 {
            lam = OwnedValue::Partial(lam, args).pack_new(alloc)?;
        }
        // Invoke the lambda into a thunk and return that
        let thunk= OwnedValue::Thunk(lam).pack_new(alloc)?;
        Ok(thunk)
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
    let mut new_graph = CodeGraph::default();
    // a new symbol environment
    let mut new_locals = TranspileEnv::new();
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
    println!("transpiling");
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
            let ptr = b.get_value()?.transpile_with( 
                    &TranspileContext {
                        locals: &new_locals,
                        graph: ctx.graph,
                        store: ctx.store
                    })?;
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
            Error(_) => Err(TranspileError {})
        }
    }
}

#[derive(Debug)]
pub struct CompileEnv<'e> {
    symbols: HashMap<&'e str, CompRef>
}

impl<'e> Clone for CompileEnv<'e> {
    fn clone(&self) -> Self {
        Self { symbols: self.symbols.clone() }
    }
}


impl<'e> CompileEnv<'e> {
    pub fn new() -> Self {
        Self { symbols: HashMap::new() }
    }
    pub fn get(&self, sym: &'e str) -> Option<CompRef> {
        let s = self.symbols.get(&sym)?;
        Some(*s)
    }
    pub fn add(&mut self, sym: &'e str, val: CompRef) {
        self.symbols.insert(sym, val);
    }
}