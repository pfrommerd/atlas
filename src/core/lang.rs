use ordered_float::NotNan;
use std::collections::{HashMap, HashSet};
use std::iter::IntoIterator;

// The core-language definition, very similar to
// that used by GHC. Extended slightly to support the extra features
// we have for adding dependencies. All types are represented as tuples
// but it preserves interface information.

#[derive(Clone, Debug)]
pub enum Literal {
    Unit,
    Bool(bool),
    Int(i64),
    Float(NotNan<f64>),
    // contains the type this string literal should be treated as
    String(String), 
    Char(char)
}


#[derive(Clone, Debug)]
pub enum Bind {
    NonRec(Symbol, Box<Expr>), // bind symbol  to expr
    Rec(Vec<(Symbol, Expr)>) // bind multiple symbols, expressions recursively
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Id {
    name: String,
    disamb: u64
}

impl Id {
    pub fn new(name: String, disamb: u64) -> Self {
        Id { name, disamb }
    }
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Symbol {
    // contains optional debugging information
    pub id: Id, // the variable Id
    pub left_assoc: bool,
    pub priority: i8
}

impl Symbol {
    pub fn new(id: Id) -> Self {
        Symbol{id: id, left_assoc: false, priority: 0}
    }
    pub fn new_op(name: String, disamb: u64,
                  left_assoc: bool, priority: i8) -> Self {
        Symbol {
            id: Id::new(name, disamb),
            left_assoc, priority,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Type {
    Any, // : _ used when nothing about the type is known
    Star,
    // Conjunction(Box<Expr>, Box<Expr>),
    Field(String, Box<Expr>), // A build-in trait of having a particular field
    Arrow(Box<Expr>, Box<Expr>),
    Tuple(Vec<Expr>),
    Record(Vec<(String, Expr)>),
    Variant(Vec<(String, Vec<Expr>)>),
    Module(Vec<(bool, String, Expr)>)
}

/**
 * The desugared version of the language
 */
#[derive(Clone, Debug)]
pub enum Expr {
    Type(Type), 
    Var(Id),
    Lit(Literal),
    Pack{tag: u16, arity: usize, res_type: Box<Expr>},
    Case{expr: Box<Expr>, case_sym: Symbol,
         alt: Vec<Alter>},

    // contrains something to be of a particular type
    // this will potentially chainge the pack structure
    // that particular type, and will panic if it fails
    Constrain{expr: Box<Expr>, expr_type: Box<Expr>},
    // Will extract a particular index from a packed expression
    Unpack{expr: Box<Expr>, idx: usize},

    Lam{var: Symbol, body: Box<Expr>},
    Let(Bind, Box<Expr>),
    App(Box<Expr>, Box<Expr>), // LHS is lambda, RHS is arg
    Bad // a bad expression
}

#[derive(Clone, Debug)]
pub enum AlterCond {
    Tag(u16), Lit(Literal), 
    Default
}

#[derive(Clone, Debug)]
pub struct Alter { // a case alternative
    cond: AlterCond,
    expr: Expr // expression for this alternative
}

impl Expr {
    // will turn a bunch of binds and an expression into
    // a LetIn(bind0, LetIn(bind[1], ...., exp))
    pub fn chain_binds(_binds: Vec<Bind>, _body: Expr) -> Expr {
        panic!("TODO")
    }

    // will chain a bunch of 
    pub fn chain_app(func: Expr, args: Vec<Expr>) -> Expr {
        let mut x = func;
        for arg in args.into_iter() {
            x = Expr::App(
                Box::new(x),
                Box::new(arg)
            );
        }
        x
    }

    pub fn traverse<F: FnMut(&Expr) -> ()>(&self, func: &mut F) {
        func(self);
        use Expr::*;
        match &self {
            Pack{tag:_, arity:_, res_type} => {
                res_type.traverse(func);
            },
            Type(t) => {
                use self::Type::*;
                match t {
                    Arrow(left, right) => {
                        left.traverse(func);
                        right.traverse(func)
                    },
                    Tuple(cont) => cont.iter()
                        .for_each(|e| e.traverse(func)),
                    Record(cont) => cont.iter()
                        .for_each(|(_, e)| e.traverse(func)),
                    Variant(cont) => cont.iter()
                        .for_each(|(_, c)| c.iter().for_each(|e| e.traverse(func))),
                    Module(cont) => cont.iter()
                        .for_each(|(_, _, e)| e.traverse(func)),
                    _ => ()
                }
            },
            Case{expr, case_sym:_, alt} => {
                expr.traverse(func);
                for Alter{cond:_, expr} in alt {
                    expr.traverse(func);
                }
            },
            Let(binds, body) => {
                match binds {
                    Bind::NonRec(_, val) => {
                        val.traverse(func);
                    },
                    Bind::Rec(recs) => recs.iter().for_each(|(_, val)| {
                        val.traverse(func);
                    })
                }
                body.traverse(func)
            },
            App(lam, arg) => {
                lam.traverse(func);
                arg.traverse(func);
            }
            _ => ()
        }
    }

    pub fn filter_contained<'a>(&'a self, ids: &HashSet<Id>) -> HashSet<Id> {
        let mut s = HashSet::new();
        self.traverse(&mut |expr : &Expr| {
            match expr {
                Expr::Var(id) => if ids.contains(id) { s.insert(id.clone()); },
                _ => ()
            }
        });
        s
    }
}


// A symbol environment is for turning names into
// unique symbols that don't shadow each other
pub struct SymbolEnv<'p> {
    parent: Option<&'p SymbolEnv<'p>>,
    pub symbols: HashMap<String, Symbol>
}

impl<'p> SymbolEnv<'p> {
    pub fn new() -> Self {
        Self { parent: None, symbols: HashMap::new() }
    }

    pub fn child(parent: &'p SymbolEnv<'p>) -> Self {
        Self { parent: Some(parent), symbols: HashMap::new() }
    }

    pub fn default() -> Self {
        let mut env = Self::new();
        env.add(Symbol::new_op(String::from("*"), 0, true, 0));
        env.add(Symbol::new_op(String::from("/"), 0, true, 0));
        env.add(Symbol::new_op(String::from("+"), 0, true, 1));
        env.add(Symbol::new_op(String::from("-"), 0, true, 1));
        env
    }

    pub fn extend(&mut self, child: HashMap<String, Symbol>) {
        self.symbols.extend(child)
    }

    pub fn add(&mut self, sym: Symbol) {
        self.symbols.insert(sym.id.name.clone(), sym);
    }

    pub fn next_id(&self, s: String) -> Id {
        match self.lookup(s.as_str()) {
            Some(Symbol{id, left_assoc:_, priority:_}) => Id::new(s, id.disamb + 1),
            None => Id::new(s, 0)
        }
    }

    pub fn lookup<'a>(&'a self, name: &str) -> Option<&'a Symbol> {
        match self.symbols.get(name) {
            Some(s) => Some(s),
            None => match self.parent {
                Some(parent) => parent.lookup(name),
                None => None
            }
        }
    }
}