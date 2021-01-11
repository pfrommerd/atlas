use ordered_float::NotNan;
use std::collections::{HashMap, HashSet};

use crate::parse::ast:: {
    Expr as AstExpr,
    Literal as AstLiteral,
    LetBinding,
    Span
};

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

impl Literal {
    fn from_ast(lit: AstLiteral) -> Literal {
        match lit {
            AstLiteral::Unit => Literal::Unit,
            AstLiteral::Bool(b) => Literal::Bool(b),
            AstLiteral::Int(i) => Literal::Int(i),
            AstLiteral::Float(f) => Literal::Float(f),
            AstLiteral::String(s) => Literal::String(s),
            AstLiteral::Char(c) => Literal::Char(c)
        }
    }
}

#[derive(Clone, Debug)]
pub enum Binds {
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
    left_assoc: bool,
    priority: i8
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
    Unknown,
    Star,
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
         alt: Vec<Alter>, res_type: Box<Expr>},
    // contrains something to be of a particular type
    Constrain{expr: Box<Expr>, expr_type: Box<Expr>},
    Lam{var: Symbol, body: Box<Expr>},
    Let(Binds, Box<Expr>),
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
    pub fn transpile_expr(env: &SymbolEnv, ast: &AstExpr) -> Expr {
        match ast {
            AstExpr::Literal(_, lit) => {
                Expr::Lit(Literal::from_ast(lit.clone()))
            },
            AstExpr::Identifier(_, ident) => {
                if let Some(sym) = env.lookup(ident) {
                    Expr::Var(sym.id.clone())
                } else {
                    Expr::Bad
                }
            },
            AstExpr::Infix(_, args, ops) => {
                // find splitting op
                if args.len() == 1 { 
                    Expr::transpile_expr(env, &args[0])
                } else {
                    // find the "root" of the operation
                    // tree by looking for the last in the OOP
                    let mut lowest_priority = -1;
                    let mut left_assoc = false;
                    let mut split_idx = 0;
                    for (idx, op) in ops.iter().enumerate() {
                        if let Some(sym) = env.lookup(op) {
                            if lowest_priority < sym.priority {
                                lowest_priority = sym.priority;
                                left_assoc = sym.left_assoc;
                                split_idx = idx;
                            }
                            if lowest_priority == sym.priority && left_assoc {
                                split_idx = idx;
                            }
                        }
                    }
                    let mut largs = args.clone();
                    let rargs = largs.split_off(split_idx + 1);

                    let mut lops = ops.clone();
                    let mut cops = lops.split_off(split_idx);
                    let rops = cops.split_off(1);

                    let op = cops[0];
                    let left = AstExpr::Infix(Span::new(0, 0), largs, lops);
                    let right = AstExpr::Infix(Span::new(0, 0), rargs, rops);
                    if let Some(sym) = env.lookup(op) {
                        Expr::App(Box::new(Expr::App(
                            Box::new(Expr::Var(sym.id.clone())), 
                            Box::new(Expr::transpile_expr(env, &left))
                        )), Box::new(Expr::transpile_expr(env, &right)))
                    } else {
                        Expr::Bad
                    }
                }
            },
            AstExpr::App(_, func, args) => {
                let mut x = Expr::transpile_expr(env, func);
                for arg in args.iter() {
                    x = Expr::App(
                        Box::new(x),
                        Box::new(Expr::transpile_expr(env, arg))
                    );
                }
                x
            },
            AstExpr::LetIn(_, lets, value) => {
                let mut nenv = SymbolEnv::child(env);
                let new_symbols = lets.create_symbols(&mut nenv);
                // compile the body, assuming the symbols are available
                let body_expr = Expr::transpile_expr(&nenv, &value);
                // Make them all recursive
                // The recursive to non-recursive binding optimization
                // happens at a later phase
                let mut bindings : Vec<(Symbol, Expr)> = Vec::new();
                for (i, (binding, mut symbols)) in lets.bindings.iter().zip(new_symbols.into_iter()).enumerate() {
                    match binding {
                        LetBinding::Pattern(_, pat, val) => {
                            let val_expr = Expr::transpile_expr(env, val);
                            if symbols.len() == 1 {
                                bindings.push((symbols.pop().unwrap(), pat.deconstruct(0, val_expr)))
                            } else {
                                // unse an intermediate id to then destructure
                                let val_id = env.next_id(format!("__{}", i));
                                // deconstruct each of the symbols individually
                                for (si, s) in symbols.into_iter().enumerate() {
                                    bindings.push((s, 
                                        pat.deconstruct(si, Expr::Var(val_id.clone()))));
                                }
                                // put the value binding in
                                bindings.push((Symbol{id:val_id, left_assoc: false, priority: 0}, val_expr));
                            }
                        },
                        LetBinding::Function(_, _name, _, _args, _body) => {
                            panic!("Function bind not yet implemented!");
                        },
                        LetBinding::Error(_) => return Expr::Bad
                    }
                }
                if bindings.is_empty() { // if no symbols were actually bound, just return the body
                    body_expr
                } else {
                    Expr::Let(Binds::Rec(bindings), Box::new(body_expr))
                }
            }
            AstExpr::Module(_span, _module) => {
                // Construct the module type first
                panic!("Modules not supported!")
            },
            _ => panic!("Unhandled ast type!")
        }
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
            Case{expr, case_sym:_, alt, res_type} => {
                expr.traverse(func);
                for Alter{cond:_, expr} in alt {
                    expr.traverse(func);
                }
                res_type.traverse(func);
            },
            Let(binds, body) => {
                match binds {
                    Binds::NonRec(_, val) => {
                        val.traverse(func);
                    },
                    Binds::Rec(recs) => recs.iter().for_each(|(_, val)| {
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
    symbols: HashMap<String, Symbol>
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