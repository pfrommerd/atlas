use super::graph::{
    Graph, GraphPtr, Node, NodePtr, GraphCollection,
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

trait Transpile<'e> {
    fn transpile(&self, syms: &SymbolEnv<'e>, graph : &Graph<'e>, 
                    graphs: &GraphCollection<'e>) -> Result<NodePtr, TranspileError>;
}

impl<'e> Transpile<'e> for PrimitiveReader<'e> {
    fn transpile(&self, _syms: &SymbolEnv<'e>, graph: &Graph<'e>,
                    _graphs: &GraphCollection<'e>) -> Result<NodePtr, TranspileError> {
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
        Ok(graph.insert(Node::Primitive(p)))
    }
}

fn transpile_lambda<'e>(expr: &ExprReader<'e>, syms: &SymbolEnv<'e>,
                        graph: &Graph<'e>, graphs: &GraphCollection<'e>) 
                            -> Result<NodePtr, TranspileError> {
    let lam = match expr.which().unwrap() {
        ExprWhich::Lam(l) => l,
        _ => panic!("Must supply lambda")
    };
    let fv = expr.free_variables(&HashSet::new());

    let entry = graphs.alloc();
    let graph_ptr = GraphPtr::from(entry.key());

    let mut new_graph = Graph::default();

    // a new symbol environment
    let mut new_syms = SymbolEnv::new();

    let mut args = Vec::new();

    // set up all of the free variables
    for sym in fv.iter() {
        // If the symbol can be directly expressed by a graph pointer
        // or primitive pointer, auto-lower it
        let mut sval = syms.get(*sym).ok_or(TranspileError{})?;
        // check if sval should be lifted
        sval = if let SymbolValue::Ptr(p) = sval {
            let node = graph.get(p).unwrap();
            match &*node {
                Node::Func(g) => SymbolValue::Func(*g),
                Node::Primitive(p) => SymbolValue::Primitive(p.clone()),
                _ => sval
            }
        } else { sval };

        if let SymbolValue::Ptr(p) = sval {
            let ptr = new_graph.add_input(InputType::Lifted);
            new_syms.add(*sym, SymbolValue::Ptr(ptr));
            args.push((ApplyType::Lifted, p));
        } else {
            // otherwise just copy the env mapping through
            new_syms.add(*sym, sval)
        }
    }
    // set up all of the parameters
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
        new_syms.add(sym, SymbolValue::Ptr(ptr));
    }
    // compile body into graph
    let res = lam.get_body()?.transpile(&new_syms, &new_graph, graphs)?;
    new_graph.set_output(res);
    entry.insert(new_graph);

    // now that the graph has been constructed
    // set it into the tree
    let func = graph.insert(Node::Func(graph_ptr));
    if args.len() == 0 {
        Ok(func)
    } else {
        Ok(graph.insert(Node::Apply(func, args)))
    }
}

fn transpile_apply<'e>(expr: &ExprReader<'e>, syms: &SymbolEnv<'e>,
                        graph: &Graph<'e>, graphs: &GraphCollection<'e>) 
                            -> Result<NodePtr, TranspileError> {
    let apply= match expr.which().unwrap() {
        ExprWhich::App(app) => app,
        _ => panic!("Must supply apply")
    };
    let mut args = Vec::new();
    let e = apply.get_lam()?.transpile(syms, graph, graphs)?;
    for a in apply.get_args()?.iter() {
        use ApplyWhich::*;
        let arg = a.get_value()?.transpile(syms, graph, graphs)?;
        let t = match a.which()? {
            Pos(_) => ApplyType::Pos,
            Key(s) => ApplyType::Key(s?),
            VarPos(_) => ApplyType::VarPos,
            VarKey(_) => ApplyType::VarKey
        };
        args.push((t, arg));
    }
    Ok(graph.insert(Node::Apply(e, args)))
}

fn transpile_let<'e>(expr: &ExprReader<'e>, syms: &SymbolEnv<'e>,
                        graph: &Graph<'e>, graphs: &GraphCollection<'e>)
                            -> Result<NodePtr, TranspileError> {
    let l = match expr.which().unwrap() {
        ExprWhich::Let(l) => l,
        _ => panic!("Must supply apply")
    };
    let binds = l.get_binds()?.get_binds()?;
    let mut new_syms = syms.clone();
    for b in binds {
        let s = b.get_symbol()?;
        let sym = (s.get_name()?, s.get_disam());
        let ptr = b.get_value()?.transpile(&new_syms, graph, graphs)?;
        new_syms.add(sym, SymbolValue::Ptr(ptr));
    }
    // transpile the body
    l.get_body()?.transpile(&new_syms, graph, graphs)
}

impl<'e> Transpile<'e> for ExprReader<'e> {
    fn transpile(&self, syms: &SymbolEnv<'e>, graph: &Graph<'e>,
                    graphs: &GraphCollection<'e>) -> Result<NodePtr, TranspileError> {
        use ExprWhich::*;
        let t = self.which();
        match t? {
            Id(s) => {
                let sym = s?;
                syms.lookup((sym.get_name()?, sym.get_disam()), graph)
                        .ok_or(TranspileError{})
            },
            Lam(_) => transpile_lambda(self, syms, graph, graphs),
            Let(_) => transpile_let(self, syms, graph, graphs),
            App(_) => transpile_apply(self, syms, graph, graphs),
            _ => panic!("Foo")
        }
    }
}

#[derive(Clone)]
pub enum SymbolValue<'e> {
    Ptr(NodePtr),
    // functions and primitive types
    // are auto-inlined
    Func(GraphPtr),
    Primitive(Primitive<'e>),
}

pub struct SymbolEnv<'e> {
    symbols: RwLock<HashMap<(&'e str, DisambID), SymbolValue<'e>>>,
}

impl<'e> Clone for SymbolEnv<'e> {
    fn clone(&self) -> Self { 
        let map = self.symbols.read().unwrap().clone();
        Self { symbols : RwLock::new(map) }
    }
}

impl<'e> SymbolEnv<'e> {
    pub fn new() -> Self {
        Self { symbols: RwLock::new(HashMap::new()) }
    }
    pub fn get(&self, sym: (&str, DisambID)) -> Option<SymbolValue<'e>> {
        self.symbols.read().unwrap().get(&sym).map(|x| x.clone())
    }
    // will do an get and if it is not a pointer, will automically
    // insert an appropriate node into the graph and return a pointer
    pub fn lookup(&self, sym: (&'e str, DisambID), graph: &Graph<'e>) -> Option<NodePtr> {
        let mut s= self.symbols.write().unwrap();
        let val = s.get(&sym)?;
        if let SymbolValue::Ptr(p) = val { return Some(*p); }
        let ptr = match val {
            SymbolValue::Ptr(p) => *p, 
            SymbolValue::Func(g) => graph.insert(Node::Func(*g)), 
            SymbolValue::Primitive(p) => graph.insert(Node::Primitive(p.clone()))
        };
        s.insert(sym, SymbolValue::Ptr(ptr));
        Some(ptr)
    }
    pub fn add(&mut self, sym: (&'e str, DisambID), val: SymbolValue<'e>) {
        self.symbols.write().unwrap().insert(sym, val);
    }
}