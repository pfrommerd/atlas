use super::graph::{
    OpGraph, OpNode, OpNodeRef, OpGraphRef, OpGraphCollection,
    Primitive, InputType, ApplyType
};

use crate::core::lang::{
    DisambID, ExprReader, ExprWhich, ApplyWhich,
    PrimitiveReader, PrimitiveWhich, ParamWhich
};

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

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

pub struct TranspileContext<'e, 'l> {
    pub locals : &'l LocalEnv<'e>,
    pub lam: &'l LambdaEnv<'e>,
    pub graph: &'l OpGraph<'e>,
    pub collection: &'l OpGraphCollection<'e>
}

trait Transpile<'e> {
    fn transpile(&self, ctx: &TranspileContext<'e, '_>) 
            -> Result<OpNodeRef, TranspileError>;
}

impl<'e> Transpile<'e> for PrimitiveReader<'e> {
    fn transpile(&self, ctx: &TranspileContext<'e, '_>) 
            -> Result<OpNodeRef, TranspileError> {
        // build a value in the storage
        use PrimitiveWhich::*;
        let p = match self.which()? {
            Unit(_) => Primitive::Unit,
            Bool(b) => Primitive::Bool(b),
            Int(i) => Primitive::Int(i),
            Float(f) => Primitive::Float(f),
            Char(c) => Primitive::Char(std::char::from_u32(c).ok_or(TranspileError{})?),
            String(s) => Primitive::String(s?),
            Buffer(b) => Primitive::Buffer(b?),
            EmptyList(_) => Primitive::EmptyList,
            EmptyTuple(_) => Primitive::EmptyTuple,
            EmptyRecord(_) => Primitive::EmptyRecord
        };
        Ok(ctx.graph.ops.insert(OpNode::Primitive(p)))
    }
}

fn transpile_lambda<'e>(expr: &ExprReader<'e>, ctx: &TranspileContext<'e, '_>)
                            -> Result<OpNodeRef, TranspileError> {
    let lam = match expr.which().unwrap() {
        ExprWhich::Lam(l) => l,
        _ => panic!("Must supply lambda")
    };
    let fv = expr.free_variables(&HashSet::new());

    // allocate an empty node
    let graph_ptr = ctx.collection.graphs.empty();

    let mut new_graph = OpGraph::default();
    // a new symbol environment
    let mut new_lam_env= LambdaEnv::new();
    let mut args = Vec::new();

    // set up all of the free variables
    for sym in fv.iter() {
        // check if it is for a lowered value and pass directly through
        if let Some(v) = ctx.lam.get_lowered(*sym) {
            new_lam_env.add_lowered(*sym, v)
        } else {
            // get the pointer
            let ptr = ctx.locals.get(*sym).or_else(
                || ctx.lam.get(*sym)
            ).ok_or(TranspileError{})?;
            // Check if we can lower the value
            let val = ctx.graph.ops.get(ptr).and_then(
                |node| {
                    match node {
                    OpNode::Func(f) => Some(LoweredValue::Func(*f)),
                    OpNode::Primitive(p) => Some(LoweredValue::Primitive(p.clone())),
                    _ => None
                    }
                });
            match val {
                Some(v) => new_lam_env.add_lowered(*sym, v),
                None => {
                    // lift the symbol into the lambda arguments
                    let inp_ptr = new_graph.add_input(InputType::Lifted);
                    new_lam_env.add(*sym, inp_ptr);
                    // we want to later apply the lifted argument
                    args.push((ApplyType::Lifted, ptr))
                }
            }
        }
    }
    // set up all of the actual inputs
    let params = lam.get_params()?;
    for p in params.iter() {
        let s = p.get_symbol()?;
        let sym = (s.get_name()?, s.get_disam());
        use ParamWhich::*;
        let t = match p.which()? {
            Pos(_) => InputType::Pos,
            Named(s) => InputType::Key(s?),
            Optional(s) => InputType::Optional(s?),
            VarPos(_) => InputType::VarPos,
            VarKey(_) => InputType::VarKey
        };
        let ptr = new_graph.add_input(t);
        new_lam_env.add(sym, ptr);
    }
    // compile body into graph
    let res = lam.get_body()?.transpile(
    &TranspileContext {
            locals: &LocalEnv::new(),
            lam: &new_lam_env,
            graph: &new_graph,
            collection: ctx.collection
        })?;
    // add a force since we don't know
    // if the result will be whnf (it may be an unforced invoked lambda)
    // during optimization passes we will remove this force if it
    // becomes redundant
    let force = new_graph.ops.insert(OpNode::Force(res));
    new_graph.set_output(force);
    ctx.collection.graphs.insert_into(graph_ptr, new_graph);

    // now that the graph has been constructed
    // set it into the tree
    let func = ctx.graph.ops.insert(OpNode::Func(graph_ptr));
    if args.len() == 0 {
        Ok(func)
    } else {
        Ok(ctx.graph.ops.insert(OpNode::Apply(func, args)))
    }
}

fn transpile_apply<'e>(expr: &ExprReader<'e>, ctx: &TranspileContext<'e, '_>)
                            -> Result<OpNodeRef, TranspileError> {
    let apply= match expr.which().unwrap() {
        ExprWhich::App(app) => app,
        _ => panic!("Must supply apply")
    };
    let mut args = Vec::new();
    let e = apply.get_lam()?.transpile(ctx)?;
    for a in apply.get_args()?.iter() {
        use ApplyWhich::*;
        let arg = a.get_value()?.transpile(ctx)?;
        let t = match a.which()? {
            Pos(_) => ApplyType::Pos,
            Key(s) => ApplyType::Key(s?),
            VarPos(_) => ApplyType::VarPos,
            VarKey(_) => ApplyType::VarKey
        };
        args.push((t, arg));
    }
    Ok(ctx.graph.ops.insert(OpNode::Apply(e, args)))
}

fn transpile_invoke<'e>(expr: &ExprReader<'e>, ctx: &TranspileContext<'e, '_>)
                            -> Result<OpNodeRef, TranspileError> {
    let i = match expr.which().unwrap() {
        ExprWhich::Invoke(i) => i?,
        _ => panic!("Must supply apply")
    };
    let lam = i.transpile(ctx)?;
    Ok(ctx.graph.ops.insert(OpNode::Invoke(lam)))
}

fn transpile_let<'e>(expr: &ExprReader<'e>, ctx: &TranspileContext<'e, '_>)
                            -> Result<OpNodeRef, TranspileError> {
    let l = match expr.which().unwrap() {
        ExprWhich::Let(l) => l,
        _ => panic!("Must supply apply")
    };
    let binds = l.get_binds()?.get_binds()?;
    let new_locals = if l.get_binds()?.get_rec() {
        panic!("Cannot handle recursive bindings yet")
    } else {
        let mut new_locals= ctx.locals.clone();
        for b in binds {
            let s = b.get_symbol()?;
            let sym = (s.get_name()?, s.get_disam());
            let ptr = b.get_value()?.transpile(&TranspileContext {
                locals: &new_locals,
                lam: ctx.lam,
                graph: ctx.graph,
                collection: ctx.collection
            })?;
            new_locals.add(sym, ptr);
        }
        new_locals
    };
    // transpile the body
    l.get_body()?.transpile(&TranspileContext {
        locals: &new_locals,
        lam: ctx.lam,
        graph: ctx.graph,
        collection: ctx.collection
    })
}


fn transpile_builtin<'e>(expr: &ExprReader<'e>, ctx: &TranspileContext<'e, '_>)
                            -> Result<OpNodeRef, TranspileError> {
    let b = match expr.which().unwrap() {
        ExprWhich::InlineBuiltin(b) => b,
        _ => panic!("Must supply apply")
    };
    let s = b.get_op()?;
    let mut args = Vec::new();
    for a in b.get_args()?.iter() {
        args.push(a.transpile(ctx)?);
    }
    Ok((&ctx).graph.ops.insert(OpNode::Builtin(s, args)))
}

impl<'e> Transpile<'e> for ExprReader<'e> {
    fn transpile(&self, ctx: &TranspileContext<'e, '_>)
                    -> Result<OpNodeRef, TranspileError> {
        use ExprWhich::*;
        let t = self.which();
        match t? {
            Id(s) => {
                let sym = s?;
                let s = (sym.get_name()?, sym.get_disam());
                ctx.locals.get(s).or_else(
                    || ctx.lam.resolve(s, ctx.graph)
                ).ok_or(TranspileError {})
            },
            Literal(p) => p?.transpile(ctx),
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

#[derive(Clone)]
pub enum LoweredValue<'e> {
    Func(OpGraphRef),
    Primitive(Primitive<'e>),
}

#[derive(Clone, Default)]
struct LambdaMaps<'e> {
    pub symbols: HashMap<(&'e str, DisambID), OpNodeRef>,
    pub lowered: HashMap<(&'e str, DisambID), LoweredValue<'e>>
}

// A lambda env handles
// lowering across lambda boundaries
// while symbol envs are constructed
// through let statements
pub struct LambdaEnv<'e> {
    maps: RwLock<LambdaMaps<'e>>
}

impl<'e> LambdaEnv<'e> {
    pub fn new() -> Self {
        Self { maps: RwLock::new(LambdaMaps::default()) }
    }
    pub fn resolve(&self, sym: (&'e str, DisambID), graph: &OpGraph<'e>) -> Option<OpNodeRef> {
        let mut s = self.maps.write().unwrap();
        let res =  s.symbols.get(&sym);
        if let Some(r) = res { return Some(*r); }
        // otherwise check the lowering map
        let low = s.lowered.get(&sym)?;
        let ptr = graph.ops.insert(match low {
            LoweredValue::Func(f) => OpNode::Func(*f),
            LoweredValue::Primitive(p) => OpNode::Primitive(p.clone())
        });
        s.symbols.insert(sym, ptr);
        Some(ptr)
    }
    pub fn get_lowered(&self, sym: (&'e str, DisambID)) -> Option<LoweredValue<'e>> {
        let s = self.maps.read().unwrap();
        s.lowered.get(&sym).map(|x| x.clone())
    }
    // does not do lowered value resolution
    pub fn get(&self, sym: (&'e str, DisambID)) -> Option<OpNodeRef> {
        let s = self.maps.read().unwrap();
        s.symbols.get(&sym).map(|x| *x)
    }
    pub fn add(&mut self, sym: (&'e str, DisambID), val: OpNodeRef) {
        self.maps.write().unwrap().symbols.insert(sym, val);
    }
    pub fn add_lowered(&mut self, sym: (&'e str, DisambID), val: LoweredValue<'e>) {
        self.maps.write().unwrap().lowered.insert(sym, val);
    }
}

pub struct LocalEnv<'e> {
    symbols: RwLock<HashMap<(&'e str, DisambID), OpNodeRef>>
}

impl<'e> Clone for LocalEnv<'e> {
    fn clone(&self) -> Self {
        let s = self.symbols.read().unwrap();
        Self { symbols: RwLock::new(s.clone())}
    }
}


impl<'e> LocalEnv<'e> {
    pub fn new() -> Self {
        Self { symbols: RwLock::new(HashMap::new()) }
    }
    pub fn get(&self, sym: (&'e str, DisambID)) -> Option<OpNodeRef> {
        let s = self.symbols.read().unwrap();
        let s= s.get(&sym)?;
        Some(*s)
    }
    pub fn add(&mut self, sym: (&'e str, DisambID), val: OpNodeRef) {
        self.symbols.write().unwrap().insert(sym, val);
    }
}