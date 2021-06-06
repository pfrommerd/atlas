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
pub enum Atom {
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

#[derive(Clone, Debug)]
pub enum Format {
    Fields(Vec<String>),
    Variant(Vec<String>),
    Tuple(u16)
}

/**
 * The desugared version of the language
 */
#[derive(Clone, Debug)]
pub enum Expr {
    Atom(Atom),
    Pack(u16, usize, Format),
    // builtin lambda types
    Case(Option<Symbol>, Vec<Alter>),
    As(Format), // will coerce to a particular format
    Coerce, // will coerce to a particular type
    Unpack, // index into a pack
    TypeOf, // typeof operator
    // generic lambda with symbol, body
    Lam(Symbol, Atom),
    Let(Bind, Atom),
    App(Atom, Atom), // LHS is lambda, RHS is arg
    Bad // a bad expression
}

#[derive(Clone, Debug)]
pub enum Alter {
    Data(u16, Expr),
    Lit(Literal, Expr), 
    Default(Expr)
}

impl Expr {
    pub fn as_atom(self) -> Atom {
        Atom::Expr(Box::new(self))
    }
    // will turn a bunch of binds and an expression into
    // a LetIn(bind0, LetIn(bind[1], ...., exp))
    pub fn chain_binds(_binds: Vec<Bind>, _body: Expr) -> Expr {
        panic!("TODO")
    }

    // will chain a bunch of applies
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
            Case(case_sym, alt) => {
                let mut i = ignore.clone();
                if let Some(sym) = case_sym {
                    i.insert(sym.clone());
                }
                let mut fv = HashSet::new();
                for a in alt {
                    let expr = match a {
                        Alter::Data(_, e) => e,
                        Alter::Lit(_, e) => e,
                        Alter::Default(e) => e 
                    };
                    let h = expr.free_variables(ignore);
                    fv.extend(h);
                }
                fv
            }
            Lam(var, body) => {
                let mut i = ignore.clone();
                i.insert(var.clone());
                body.free_variables(&i)
            },
            Let(bind, body) => {
                let mut i = ignore.clone();
                let mut sym = HashSet::new();
                match bind {
                    Bind::NonRec(s, val) => {
                        sym.extend(val.free_variables(ignore));
                        i.insert(s.clone());
                    },
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