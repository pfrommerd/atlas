pub mod op_graph;

#[cfg(test)]
mod test;

pub use op_graph::{
    CodeGraph, OpNode, NodeRef, MatchCase 
};
use crate::store::Storable;
use crate::{Error, ErrorKind};
use crate::core::FreeVariables;
use crate::core::lang::{Var, Lambda, App, Literal, LetIn, Bind, Invoke, Builtin, Match, Case, Expr};
use crate::store::Storage;
use crate::store::op::BuiltinOp;
use std::collections::{HashMap, HashSet};

pub type Env<'s, H> = HashMap<String, H>;

pub trait Compile : FreeVariables {
    // This should transpile to a WNHF-forced representation
    fn compile_with<'s, S: Storage + 's>
            (&self, store: &'s S, env: &CompileEnv<'_>, 
                graph: &mut CodeGraph<S::Handle<'s>>) -> Result<NodeRef, Error>;

    // Transpile is a top-level callable. It takes a store and a map of bound variables in the store
    // and will transpile the expression to a thunk with the given env bound
    // and the compref returned by compile_into getting returned
    fn compile<'s, S: Storage + 's>(&self, store: &'s S, env: &Env<'s, S::Handle<'s>>)
                        -> Result<CodeGraph<S::Handle<'s>>, Error> {
        let mut graph = CodeGraph::default();
        let mut cenv= CompileEnv::new();
        let free = self.free_variables(&HashSet::new());
        for var in free {
            let c = env.get(var).ok_or(
                Error::new(format!("Variable {var} not found")))?;
            cenv.add(var, graph.insert(OpNode::Value(c.clone())));
        }
        // Compile into the graph we created
        let res = self.compile_with(store, &cenv, &mut graph)?;
        // Set the root to the result
        graph.set_root(res);
        Ok(graph)
    }
}

impl Compile for Var {
    fn compile_with<'s, S: Storage + 's>(&self, _: &'s S, env: &CompileEnv<'_>, 
                            _: &mut CodeGraph<S::Handle<'s>>) -> Result<NodeRef, Error> {
        env.get(self.name.as_str())
            .ok_or(Error::new_const(ErrorKind::Compile, "No such variable"))
            .cloned()
    }
}

impl Compile for Literal {
    fn compile_with<'s, S: Storage + 's>(&self, store: &'s S, _: &CompileEnv<'_>, 
                            graph: &mut CodeGraph<S::Handle<'s>>) -> Result<NodeRef, Error> {
        Ok(graph.insert(OpNode::Value(self.store_in(store)?)))
    }
}

impl Compile for LetIn {
    fn compile_with<'s, S: Storage + 's>(&self, alloc: &'s S, env: &CompileEnv<'_>, 
                            graph: &mut CodeGraph<S::Handle<'s>>) -> Result<NodeRef, Error> {
        let sub_env = match &self.bind {
        Bind::NonRec(sym, val) => {
            let mut e = env.clone();
            e.add(sym.name.as_ref(), val.compile_with(alloc, env, graph)?);
            e
        },
        Bind::Rec(binds) => {
            let mut sub_cenv = env.clone();
            // 
            let mut slots = Vec::new();
            for (s, _) in binds {
                let n = NodeRef::temp();
                sub_cenv.add(s.name.as_ref(), n.clone());
                slots.push(n);
            }
            // Compile and set the slots
            for ((_, e), mut s) in binds.iter().zip(slots.into_iter()) {
                let r = e.compile_with(alloc, &sub_cenv, graph)?;
                s.set_to(&r);
            }
            sub_cenv
        }
        };
        self.body.compile_with(alloc, &sub_env, graph)
    }
}

impl Compile for App {
    fn compile_with<'s, S: Storage + 's>(&self, alloc: &'s S, env: &CompileEnv<'_>, 
                            graph: &mut CodeGraph<S::Handle<'s>>) -> Result<NodeRef, Error> {
        // The problem with application is that we can't be sure the LHS is forced
        // so:
        // generate a new code block which internally forces the LHS lambda
        // binds the additional arguments, and returns it
        let sub_graph = {
            let mut sub_graph = CodeGraph::new();
            let sub_lam = sub_graph.insert(OpNode::Input(0));
            let sub_lam_forced = sub_graph.insert(OpNode::Force(sub_lam));

            // create args for each of the real arguments
            let mut sub_args = Vec::new();
            for (i, _) in self.args.iter().enumerate() {
                sub_args.push(sub_graph.insert(OpNode::Input(i + 1)))
            }
            let bound = sub_graph.insert(OpNode::Bind(sub_lam_forced, sub_args));
            sub_graph.set_root(bound);
            sub_graph
        };
        // turn the sub_graph into an externalgraph
        let ext = graph.insert(OpNode::Graph(sub_graph));
        // bind the original lambda + arguments to the internally generated code
        let bind_args = {
            let mut v = Vec::new();
            v.push(self.lam.compile_with(alloc, env, graph)?);
            for a in &self.args {
                v.push(a.compile_with(alloc, env, graph)?);
            }
            v
        };
        let bound = graph.insert(OpNode::Bind(ext, bind_args));
        let inv = graph.insert(OpNode::Invoke(bound)); 
        // return the invoked version of the application. That way when people
        Ok(inv)
    }
}

impl Compile for Invoke {
    fn compile_with<'s, S: Storage + 's>(&self, alloc: &'s S, env: &CompileEnv<'_>, 
                            graph: &mut CodeGraph<S::Handle<'s>>) -> Result<NodeRef, Error> {
        // The invoke op can only be applied to a forced type,
        // so generate an intermediate lambda
        let sub_graph = {
            let mut sub_graph = CodeGraph::new();
            let target = sub_graph.insert(OpNode::Input(0));
            let forced = sub_graph.insert(OpNode::Force(target));
            let invoked = sub_graph.insert(OpNode::Invoke(forced));
            sub_graph.set_root(invoked);
            sub_graph
        };
        let ext = graph.insert(OpNode::Graph(sub_graph));
        let target = self.target.compile_with(alloc, env, graph)?;
        let bound = graph.insert(OpNode::Bind(ext, vec![target]));
        let inv = graph.insert(OpNode::Invoke(bound));
        Ok(inv)
    }
}

impl Compile for Lambda {
    fn compile_with<'s, S: Storage + 's>(&self, alloc: &'s S, env: &CompileEnv<'_>, 
                            graph: &mut CodeGraph<S::Handle<'s>>) -> Result<NodeRef, Error> {
        let (sub_graph, free_args) = {
            let mut sub_graph = CodeGraph::new();
            let mut sub_env = CompileEnv::new();
            let mut free_args = Vec::new();

            let mut args = HashSet::new();
            args.extend(self.args.iter().map(|x| x.name.as_str()));
            let free_vars = self.body.free_variables(&args);
            // generate args for the free variables
            for v in free_vars {
                sub_env.add(v, sub_graph.insert(OpNode::Input(free_args.len())));
                free_args.push(env.get(v).ok_or(Error::new_const(ErrorKind::Compile, "No such variable"))?.clone());
            }
            // generate arg bindings for the actual arguments
            for (i, a) in self.args.iter().enumerate() {
                sub_env.add(a.name.as_str(), 
                    sub_graph.insert(OpNode::Input(free_args.len() + i )));
            }
            // compile into the sub env
            let res = self.body.compile_with(alloc, &sub_env, &mut sub_graph)?;
            // force the res
            let force = sub_graph.insert(OpNode::Force(res));
            sub_graph.set_root(force);
            (sub_graph, free_args)
        };
        let mut res= graph.insert(OpNode::Graph(sub_graph));
        if free_args.len() > 0 { 
            res = graph.insert(OpNode::Bind(res, free_args));
        }
        Ok(res)
    }
}

impl Compile for Match {
    fn compile_with<'s, S: Storage + 's>(&self, alloc: &'s S, env: &CompileEnv<'_>, 
                            graph: &mut CodeGraph<S::Handle<'s>>) -> Result<NodeRef, Error> {
        let (sub_graph, sub_args) = {
            let mut sub_graph = CodeGraph::new();
            let mut sub_args = Vec::new();
            sub_args.push(self.scrut.compile_with(alloc, env, graph)?);
            let scrut = sub_graph.insert(OpNode::Input(0));
            // Force the scrutinized expression
            let scrut_forced = sub_graph.insert(OpNode::Force(scrut));
            // build a match op
            let mut match_cases = Vec::new();
            for (i, case) in self.cases.iter().enumerate() {
                let branch_input = sub_graph.insert(OpNode::Input(i + 1));
                match case {
                Case::Eq(val, branch) => {
                    // First build the comparison value
                    match_cases.push(MatchCase::Eq(val.clone(), branch_input));
                    sub_args.push(branch.compile_with(alloc, env, graph)?);
                },
                Case::Tag(s, branch) => {
                    match_cases.push(MatchCase::Tag(s.clone(), branch_input));
                    sub_args.push(branch.compile_with(alloc, env, graph)?);
                },
                Case::Default(branch) => {
                    match_cases.push(MatchCase::Default(branch_input));
                    sub_args.push(branch.compile_with(alloc, env, graph)?);
                }
                }
            }
            let matched = sub_graph.insert(OpNode::Match(scrut_forced, match_cases));
            // force the result of the select call
            let res = sub_graph.insert(OpNode::Force(matched));
            sub_graph.set_root(res);
            (sub_graph, sub_args)
        };
        let ext = graph.insert(OpNode::Graph(sub_graph));
        let bound = graph.insert(OpNode::Bind(ext, sub_args));
        let inv = graph.insert(OpNode::Invoke(bound));
        Ok(inv)
    }
}

impl Compile for Builtin {
    fn compile_with<'s, S: Storage + 's>(&self, alloc: &'s S, env: &CompileEnv<'_>, 
                            graph: &mut CodeGraph<S::Handle<'s>>) -> Result<NodeRef, Error> {
        let op = if self.op == "force" {
            OpNode::Force(self.args[0].compile_with(alloc, env, graph)?)
        } else {
            OpNode::Builtin(
                BuiltinOp::try_from(self.op.as_str())?, {
                    let mut v = Vec::new();
                    for a in &self.args {
                        v.push(a.compile_with(alloc, env, graph)?)
                    }
                    v
                }
            )
        };
        Ok(graph.insert(op))
    }
}

impl Compile for Expr {
    fn compile_with<'s, S: Storage + 's>(&self, alloc: &'s S, env: &CompileEnv<'_>, 
                            graph: &mut CodeGraph<S::Handle<'s>>) -> Result<NodeRef, Error> {
        use Expr::*;
        match self {
            Var(v) => v.compile_with(alloc, env, graph),
            Literal(l) => l.compile_with(alloc, env, graph),
            LetIn(l) => l.compile_with(alloc, env, graph),
            Lambda(l) => l.compile_with(alloc, env, graph),
            App(a) => a.compile_with(alloc, env, graph),
            Invoke(i) => i.compile_with(alloc, env, graph),
            Match(m) => m.compile_with(alloc, env, graph),
            Builtin(b) => b.compile_with(alloc, env, graph)
        }
    }
}
// This is used both for transpiling lambdas, as well
// as transpiling arguments (which for laziness purposes need to be treated as 0-argument lambdas)
// and let-bound variables (which need to be lambdas/thunks in order to ensure value reuse)

#[derive(Debug)]
pub struct CompileEnv<'e> {
    symbols: HashMap<&'e str, NodeRef>
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
    pub fn get(&self, sym: &'e str) -> Option<&NodeRef> {
        let s = self.symbols.get(&sym)?;
        Some(s)
    }
    pub fn add(&mut self, sym: &'e str, val: NodeRef) {
        self.symbols.insert(sym, val);
    }
}