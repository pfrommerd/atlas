use bytes::Bytes;
use ordered_float::NotNan;
use std::collections::{HashMap, HashSet};
use std::fmt;


// The core-language definition, very similar to
// that used by GHC. Extended slightly to support the extra features
// we have for adding dependencies. All types are represented as tuples
// but it preserves interface information.

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Literal {
    Unit,
    Bool(bool),
    Int(i64),
    Float(NotNan<f64>),
    // contains the type this string literal should be treated as
    String(String),
    Char(char),
}

// we also have primitive here, but note that not all primitives
// are literals (i.e there is no buffer literal!)
#[derive(Debug, Clone, PartialEq)]
pub enum Primitive {
    Unit,
    Bool(bool),
    Int(i64),
    Float(f64),
    Char(char),
    String(String),
    Buffer(Bytes),
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum PrimitiveType {
    Unit,
    Bool,
    Int,
    Float,
    Char,
    String,
    Buffer,
}

impl Primitive {
    pub fn from_literal(l: Literal) -> Self {
        match l {
            Literal::Unit => Primitive::Unit,
            Literal::Bool(b) => Primitive::Bool(b),
            Literal::Char(c) => Primitive::Char(c),
            Literal::Int(i) => Primitive::Int(i),
            Literal::Float(f) => Primitive::Float(f.into_inner()),
            Literal::String(s) => Primitive::String(s),
        }
    }
}

impl fmt::Display for Primitive {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use Primitive::*;
        match self {
            Unit => write!(f, "()"),
            Bool(b) => write!(f, "{}", b),
            Int(i) => write!(f, "{}", i),
            Float(v) => write!(f, "{}", v),
            String(s) => write!(f, "{}", s),
            Char(c) => write!(f, "{}", c),
            Buffer(b) => write!(f, "<buf len={}>", b.len()),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Bind {
    NonRec(Symbol, Box<Expr>), // bind symbol to expr
    Rec(Vec<(Symbol, Expr)>),  // bind multiple symbols, expressions recursively
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Symbol {
    pub name: String,
    disamb: u64,
}

impl Symbol {
    pub const fn new(name: String, disamb: u64) -> Self {
        Symbol { name, disamb }
    }
}

#[derive(Clone, Debug)]
pub enum Atom {
    Id(Symbol),
    Lit(Literal),
    ListEmpty, // an empty list value
    ListCons,  // head, tail

    TupleEmpty,  // an empty tuple value
    TupleAppend, // tuple, value
    TupleJoin,   // tuple 1, tuple 2

    RecordEmpty,  // an empty record
    RecordInsert, // record, key, value
    RecordDel,    // record, key
    RecordJoin,   // record1, record2

    Variant(u16, Vec<String>), // construct variant

    Idx(usize),
    Project(String),
}

impl Atom {
    pub fn free_variables(&self, ignore: &HashSet<Symbol>) -> HashSet<Symbol> {
        match self {
            Atom::Id(s) => {
                let mut hs = HashSet::new();
                if !ignore.contains(s) {
                    hs.insert(s.clone());
                }
                hs
            }
            _ => HashSet::new(),
        }
    }
    pub fn as_body(self) -> Body {
        Body::Atom(self)
    }
}

#[derive(Clone, Debug)]
pub enum ArgType {
    Pos,
    ByName(String), // e.g foo(a: 1)
    ExpandPos,
    ExpandKeys,
}

#[derive(Clone, Debug)]
pub enum ParamType {
    Pos,              // unnamed positional argument
    Named(String),    // fn foo(a), can be passed postionally or by name
    Optional(String), // fn foo(a, ?b)
    VarPos,           // fn foo(a, ..b)
    VarKeys,          // fn foo(a, ...b)
}

/**
 * The desugared version of the language
 */
#[derive(Clone, Debug)]
pub enum Expr {
    Atom(Atom),
    App(Body, Vec<(ArgType, Expr)>), // will bind some arguments, but not do the call
    Call(Body, Vec<(ArgType, Expr)>), // can leave args blank to just do call
    Case(Option<Symbol>, Vec<(Cond, Expr)>, Body),
    Lam(Vec<(ParamType, Symbol)>, Body),
    Let(Bind, Body),
    Bad, // a bad expression
}

#[derive(Clone, Debug)]
pub enum Body {
    // To prevent each literal getting a malloc
    Atom(Atom),
    Expr(Box<Expr>),
}

impl Body {
    pub fn unwrap(self) -> Expr {
        match self {
            Body::Atom(a) => Expr::Atom(a),
            Body::Expr(e) => *e,
        }
    }
    pub fn free_variables(&self, ignore: &HashSet<Symbol>) -> HashSet<Symbol> {
        match self {
            Body::Atom(a) => a.free_variables(ignore),
            Body::Expr(e) => e.free_variables(ignore),
        }
    }
}

#[derive(Clone, Debug)]
pub enum Cond {
    Tag(String),
    ListCons, // matches list a cons element
    ListEmpty,
    Tuple(usize),              // 0 means match any kind of tuple
    Record(Vec<String>, bool), // match a record with certain fields. bool true means exact match
    Eq(Primitive),             // equal to a primitive
    Of(PrimitiveType),         // match of a particular primtiive type
    Default,
}

impl Expr {
    pub fn as_body(self) -> Body {
        match self {
            Expr::Atom(a) => Body::Atom(a),
            _ => Body::Expr(Box::new(self)),
        }
    }
    // will turn a bunch of binds and an expression into
    // a LetIn(bind0, LetIn(bind[1], ...., exp))
    pub fn chain_binds(_binds: Vec<Bind>, _body: Expr) -> Expr {
        panic!("TODO")
    }

    pub fn free_variables(&self, ignore: &HashSet<Symbol>) -> HashSet<Symbol> {
        use Expr::*;
        match self {
            Atom(a) => a.free_variables(ignore),
            Case(case_sym, alt, expr) => {
                let mut fv = expr.free_variables(ignore);
                let mut i = ignore.clone();
                if let Some(sym) = case_sym {
                    i.insert(sym.clone());
                }
                for (_, expr) in alt {
                    let h = expr.free_variables(ignore);
                    fv.extend(h);
                }
                fv
            }
            Lam(var, body) => {
                let mut i = ignore.clone();
                for (_, s) in var {
                    i.insert(s.clone());
                }
                body.free_variables(&i)
            }
            Let(bind, body) => {
                let mut i = ignore.clone();
                let mut sym = HashSet::new();
                match bind {
                    Bind::NonRec(s, val) => {
                        sym.extend(val.free_variables(ignore));
                        i.insert(s.clone());
                    }
                    Bind::Rec(v) => {
                        for (s, _) in v {
                            i.insert(s.clone());
                        }
                        for (_, val) in v {
                            sym.extend(val.free_variables(&i));
                        }
                    }
                }
                sym.extend(body.free_variables(&i));
                sym
            }
            App(lam, args) => {
                let mut l = lam.free_variables(ignore);
                l.extend(args.iter().flat_map(|(_, x)| x.free_variables(ignore)));
                l
            }
            Call(lam, args) => {
                let mut l = lam.free_variables(ignore);
                l.extend(args.iter().flat_map(|(_, x)| x.free_variables(ignore)));
                l
            }
            _ => HashSet::new(),
        }
    }
}

// A symbol environment is for turning names into
// unique symbols that don't shadow each other
pub struct SymbolMap<'p> {
    parent: Option<&'p SymbolMap<'p>>,
    pub symbols: HashMap<String, Symbol>,
}

impl<'p> SymbolMap<'p> {
    pub fn new() -> Self {
        Self {
            parent: None,
            symbols: HashMap::new(),
        }
    }

    pub fn child(parent: &'p SymbolMap<'p>) -> Self {
        Self {
            parent: Some(parent),
            symbols: HashMap::new(),
        }
    }

    pub fn extend(&mut self, child: HashMap<String, Symbol>) {
        self.symbols.extend(child)
    }

    pub fn add(&mut self, sym: Symbol) {
        self.symbols.insert(sym.name.clone(), sym);
    }

    pub fn next_id(&self, s: String) -> Symbol {
        match self.lookup(s.as_str()) {
            Some(id) => Symbol::new(s, id.disamb + 1),
            None => Symbol::new(s, 0),
        }
    }

    pub fn lookup<'a>(&'a self, name: &str) -> Option<&'a Symbol> {
        match self.symbols.get(name) {
            Some(s) => Some(s),
            None => match self.parent {
                Some(parent) => parent.lookup(name),
                None => None,
            },
        }
    }
}
