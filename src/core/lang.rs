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
    NonRec(Symbol, Box<Expr>), // bind symbol to expr
    Rec(Vec<(Symbol, Expr)>) // bind multiple symbols, expressions recursively
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Symbol {
    pub name: String,
    disamb: u64
}

impl Symbol {
    pub fn new(name: String, disamb: u64) -> Self {
        Symbol { name, disamb }
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
    Module(Vec<(bool, Symbol, Expr)>)
}

#[derive(Clone, Debug)]
pub enum Atom {
    Type(Type),
    Id(Symbol),
    Lit(Literal),
    Expr(Box<Expr>) // for recursion
}

impl Atom {
    pub fn new(e: Expr) -> Atom {
        Atom::Expr(Box::new(e))
    }
    pub fn free_variables(&self, ignore: &HashSet<Symbol>) -> HashSet<Symbol> {
        match self {
            Atom::Id(s) => {
                let mut hs = HashSet::new();
                if !ignore.contains(s) { hs.insert(s.clone()); }
                hs
            },
            _ => HashSet::new()
        }
    }
}

/**
 * The desugared version of the language
 */
#[derive(Clone, Debug)]
pub enum Expr {
    Atom(Atom),
    Pack{tag: u16, arity: usize, res_type: Atom},
    Case{expr: Atom, case_sym: Symbol,
         alt: Vec<Alter>},

    Expect{expr: Atom, expr_type: Atom},
    Cast{expr: Atom, new_type: Atom}, // will panic if not castable
    Unpack{expr: Atom, idx: usize},

    Lam{var: Symbol, body: Atom},
    Let(Bind, Atom),
    App(Atom, Atom), // LHS is lambda, RHS is arg
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
    pub fn apply(func: Expr, args: Vec<Expr>) -> Expr {
        let mut x = func;
        for arg in args.into_iter() {
            x = Expr::App(
                Atom::new(x),
                Atom::new(arg)
            );
        }
        x
    }

    pub fn free_variables(&self, ignore: &HashSet<Symbol>) -> HashSet<Symbol> {
        use Expr::*;
        match self {
            Atom(a) => a.free_variables(ignore),
            Case{expr, case_sym, alt} => {
                let mut i = ignore.clone();
                i.insert(case_sym.clone());
                let mut fv = expr.free_variables(&i);
                for Alter{cond:_, expr} in alt {
                    let h = expr.free_variables(ignore);
                    fv.extend(h);
                }
                fv
            }
            Expect{expr, expr_type} => {
                let mut fv = expr.free_variables(ignore);
                let exp = expr_type.free_variables(ignore);
                fv.extend(exp);
                fv
            },
            Cast{expr, new_type} => {
                let mut fv = expr.free_variables(ignore);
                let exp = new_type.free_variables(ignore);
                fv.extend(exp);
                fv
            },
            Unpack{expr, idx:_} => expr.free_variables(ignore),
            Lam{var, body} => {
                let mut i = ignore.clone();
                i.insert(var.clone());
                body.free_variables(&i)
            },
            App(left, right) => {
                let mut l =  left.free_variables(ignore);
                l.extend(right.free_variables(ignore));
                l
            },
            _ => HashSet::new()
        }
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
        env.add(Symbol::new(String::from("*"), 0));
        env.add(Symbol::new(String::from("/"), 0));
        env.add(Symbol::new(String::from("+"), 0));
        env.add(Symbol::new(String::from("-"), 0));
        env
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
            None => Symbol::new(s, 0)
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