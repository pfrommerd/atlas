pub mod op_graph;
pub mod pack;

#[cfg(test)]
mod test;

pub use op_graph::{
    CodeGraph, OpNode, CompRef, OpCase
};
use pack::Pack;
use crate::{Error, ErrorKind};
use crate::core::FreeVariables;
use crate::core::lang::{Var, Lambda, App, Literal, LetIn, Bind, Invoke, Builtin, Match, Case, Expr};
use crate::value::{Env, Allocator, ObjHandle, owned::{OwnedValue, Numeric}};
use std::collections::{HashMap, HashSet};


pub trait Compile : FreeVariables {
    // This should transpile to a WNHF-forced representation
    fn compile_into<'a, A: Allocator>(&self, alloc: &'a A, env: &CompileEnv<'_>, 
                            graph: &CodeGraph<'a, A>) -> Result<CompRef, Error>;

    // Transpile is a top-level callable. It takes a store and a map of bound variables in the store
    // and will transpile the expression to a thunk with the given env bound
    // and the compref returned by compile_into getting returned
    fn compile<'a, A: Allocator>(&self, alloc: &'a A, env: &Env<'a, A>)
                        -> Result<ObjHandle<'a, A>, Error> {
        let mut graph = CodeGraph::default();
        let mut cenv= CompileEnv::new();
        let mut args : Vec<ObjHandle<'a, A>> = Vec::new();

        let mut free = self.free_variables(&HashSet::new());
        for (s, v) in env.iter() {
            if free.contains(s.as_str()) {
                let inp = graph.create_input();
                cenv.add(s.as_str(), inp);
                args.push(v.clone());
                free.remove(s.as_str());
            }
        }
        // If there are still free variables, throw an error
        if !free.is_empty() { return Err(Error::new_const(ErrorKind::Compile, "Unbound variables")) }
        // Compile into the graph we created
        let res = self.compile_into(alloc, &cenv, &mut graph)?;
        // Force + return the result
        let res_force = graph.insert(OpNode::Force(res));
        graph.set_output(res_force);

        // Pack the graph
        let mut lam = graph.pack_new(alloc)?;
        log::trace!(target: "compile", "compiled to {}", lam.as_code()?.reader());
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

impl Compile for Var {
    fn compile_into<A: Allocator>(&self, _: &A, env: &CompileEnv<'_>, 
                            _: &CodeGraph<'_, A>) -> Result<CompRef, Error> {
        env.get(self.name.as_str()).ok_or(Error::new_const(ErrorKind::Compile, "No such variable"))
    }
}

impl Compile for Literal {
    fn compile_into<'a, A: Allocator>(&self, alloc: &'a A, _: &CompileEnv<'_>, 
                            graph: &CodeGraph<'a, A>) -> Result<CompRef, Error> {
        use Literal::*;
        let val = match self {
            Unit => OwnedValue::Unit,
            Int(i) => OwnedValue::Numeric(Numeric::Int(*i)),
            Float(f) => OwnedValue::Numeric(Numeric::Float(*f)),
            Bool(b) => OwnedValue::Bool(*b),
            Char(c) => OwnedValue::Char(*c),
            String(s) => OwnedValue::String(s.clone()),
            Buffer(b) => OwnedValue::Buffer(b.clone()),
            EmptyList => OwnedValue::Nil,
            EmptyTuple => OwnedValue::Tuple(Vec::new()),
            EmptyRecord => OwnedValue::Record(Vec::new())
        };
        let h = val.pack_new(alloc)?;
        Ok(graph.insert(OpNode::External(h)))
    }
}

impl Compile for LetIn {
    fn compile_into<'a, A: Allocator>(&self, alloc: &'a A, env: &CompileEnv<'_>, 
                            graph: &CodeGraph<'a, A>) -> Result<CompRef, Error> {
        let sub_env = match &self.bind {
        Bind::NonRec(sym, val) => {
            let mut e = env.clone();
            e.add(sym.name.as_ref(), val.compile_into(alloc, env, graph)?);
            e
        },
        Bind::Rec(binds) => {
            let mut sub_cenv = env.clone();
            // 
            let mut slots = Vec::new();
            for (s, _) in binds {
                let slot = graph.slot();
                sub_cenv.add(s.name.as_ref(), slot.get_ref());
                slots.push(slot);
            }
            // Compile and set the slots
            for ((_, e), s) in binds.iter().zip(slots.into_iter()) {
                let r = e.compile_into(alloc, &sub_cenv, graph)?;
                s.insert(OpNode::Indirect(r))
            }
            sub_cenv
        }
        };
        self.body.compile_into(alloc, &sub_env, graph)
    }
}

impl Compile for App {
    fn compile_into<'a, A: Allocator>(&self, alloc: &'a A, env: &CompileEnv<'_>, 
                            graph: &CodeGraph<'a, A>) -> Result<CompRef, Error> {
        // The problem with application is that we can't be sure the LHS is forced
        // so:
        // generate a new code block which internally forces the LHS lambda
        // binds the additional arguments, and returns it
        let sub_graph = {
            let mut sub_graph = CodeGraph::new();
            let sub_lam = sub_graph.create_input();
            let sub_lam_forced = sub_graph.insert(OpNode::Force(sub_lam));

            // create args for each of the real arguments
            let mut sub_args = Vec::new();
            for _ in &self.args {
                sub_args.push(sub_graph.create_input());
            }
            let bound = sub_graph.insert(OpNode::Bind(sub_lam_forced, sub_args));
            sub_graph.set_output(bound);
            sub_graph
        };
        // turn the sub_graph into an external
        let ext = graph.insert(OpNode::ExternalGraph(sub_graph));
        // bind the original lambda + arguments to the internally generated code
        let bound = graph.insert(OpNode::Bind(ext, {
            let mut v = Vec::new();
            v.push(self.lam.compile_into(alloc, env, graph)?);
            for a in &self.args {
                v.push(a.compile_into(alloc, env, graph)?);
            }
            v
        }));
        let inv = graph.insert(OpNode::Invoke(bound)); 
        // return the invoked version of the application. That way when people
        Ok(inv)
    }
}

impl Compile for Invoke {
    fn compile_into<'a, A: Allocator>(&self, alloc: &'a A, env: &CompileEnv<'_>, 
                            graph: &CodeGraph<'a, A>) -> Result<CompRef, Error> {
        // we cannot directly return an invoke of the argument, so it should
        // be called lazily (i.e only when forced)
        let sub_graph = {
            let mut sub_graph = CodeGraph::new();
            let target = sub_graph.create_input();
            let forced = sub_graph.insert(OpNode::Force(target));
            let invoked = sub_graph.insert(OpNode::Invoke(forced));
            let forced_invoke = sub_graph.insert(OpNode::Force(invoked));
            sub_graph.set_output(forced_invoke);
            sub_graph
        };
        let ext = graph.insert(OpNode::ExternalGraph(sub_graph));
        let bound = graph.insert(OpNode::Bind(ext, vec![self.target.compile_into(alloc, env, graph)?]));
        let inv = graph.insert(OpNode::Invoke(bound));
        Ok(inv)
    }
}

impl Compile for Lambda {
    fn compile_into<'a, A: Allocator>(&self, alloc: &'a A, env: &CompileEnv<'_>, 
                            graph: &CodeGraph<'a, A>) -> Result<CompRef, Error> {
        let (sub_graph, free_args) = {
            let mut sub_graph = CodeGraph::new();
            let mut sub_env = CompileEnv::new();
            let mut free_args = Vec::new();

            let mut args = HashSet::new();
            args.extend(self.args.iter().map(|x| x.name.as_str()));
            let free_vars = self.body.free_variables(&args);
            // generate args for the free variables
            for v in free_vars {
                sub_env.add(v, sub_graph.create_input());
                free_args.push(env.get(v).ok_or(Error::new_const(ErrorKind::Compile, "No such variable"))?);
            }
            // generate arg bindings for the actual arguments
            for a in self.args.iter() {
                sub_env.add(a.name.as_str(), sub_graph.create_input());
            }
            // compile into the sub env
            let res = self.body.compile_into(alloc, &sub_env, &sub_graph)?;
            // force the res
            let force = sub_graph.insert(OpNode::Force(res));
            sub_graph.set_output(force);
            (sub_graph, free_args)
        };
        let mut res= graph.insert(OpNode::ExternalGraph(sub_graph));
        if free_args.len() > 0 { 
            res = graph.insert(OpNode::Bind(res, free_args));
        }
        Ok(res)
    }
}

impl Compile for Match {
    fn compile_into<'a, A: Allocator>(&self, alloc: &'a A, env: &CompileEnv<'_>, 
                            graph: &CodeGraph<'a, A>) -> Result<CompRef, Error> {
        let (sub_graph, sub_args) = {
            let mut sub_graph = CodeGraph::new();
            let mut sub_args = Vec::new();
            sub_args.push(self.scrut.compile_into(alloc, env, graph)?);
            let scrut = sub_graph.create_input();
            // Force the scrutinized expression
            let scrut_forced = sub_graph.insert(OpNode::Force(scrut));
            // build a match op
            let mut match_cases = Vec::new();
            for case in self.cases.iter() {
                match case {
                Case::Eq(val, branch) => {
                    // First build the comparison value
                    match_cases.push(OpCase::Eq(val.clone(), sub_graph.create_input()));
                    sub_args.push(branch.compile_into(alloc, env, graph)?);
                },
                Case::Tag(s, branch) => {
                    match_cases.push(OpCase::Tag(s.clone(), sub_graph.create_input()));
                    sub_args.push(branch.compile_into(alloc, env, graph)?);
                },
                Case::Default(branch) => {
                    match_cases.push(OpCase::Default(sub_graph.create_input()));
                    sub_args.push(branch.compile_into(alloc, env, graph)?);
                }
                }
            }
            let matched = sub_graph.insert(OpNode::Match(scrut_forced, match_cases));
            // force the result of the select call
            let res = sub_graph.insert(OpNode::Force(matched));
            sub_graph.set_output(res);
            (sub_graph, sub_args)
        };
        let ext = graph.insert(OpNode::ExternalGraph(sub_graph));
        let bound = graph.insert(OpNode::Bind(ext, sub_args));
        let inv = graph.insert(OpNode::Invoke(bound));
        Ok(inv)
    }
}

impl Compile for Builtin {
    fn compile_into<'a, A: Allocator>(&self, alloc: &'a A, env: &CompileEnv<'_>, 
                            graph: &CodeGraph<'a, A>) -> Result<CompRef, Error> {
        let op = if self.op == "force" {
            OpNode::Force(self.args[0].compile_into(alloc, env, graph)?)
        } else {
            OpNode::Builtin(
                self.op.clone(), {
                    let mut v = Vec::new();
                    for a in &self.args {
                        v.push(a.compile_into(alloc, env, graph)?)
                    }
                    v
                }
            )
        };
        Ok(graph.insert(op))
    }
}

impl Compile for Expr {
    fn compile_into<'a, A: Allocator>(&self, alloc: &'a A, env: &CompileEnv<'_>, 
                            graph: &CodeGraph<'a, A>) -> Result<CompRef, Error> {
        use Expr::*;
        match self {
            Var(v) => v.compile_into(alloc, env, graph),
            Literal(l) => l.compile_into(alloc, env, graph),
            LetIn(l) => l.compile_into(alloc, env, graph),
            Lambda(l) => l.compile_into(alloc, env, graph),
            App(a) => a.compile_into(alloc, env, graph),
            Invoke(i) => i.compile_into(alloc, env, graph),
            Match(m) => m.compile_into(alloc, env, graph),
            Builtin(b) => b.compile_into(alloc, env, graph)
        }
    }
}
// This is used both for transpiling lambdas, as well
// as transpiling arguments (which for laziness purposes need to be treated as 0-argument lambdas)
// and let-bound variables (which need to be lambdas/thunks in order to ensure value reuse)

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