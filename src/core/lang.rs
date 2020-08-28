use ordered_float::NotNan;
use std::collections::HashMap;
use std::fmt;

use crate::parse::ast:: {
    Expr as AstExpr,
    Literal as AstLiteral,
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
pub enum Bind {
    NonRec(Symbol, Box<Expr>, Box<Expr>), // bind symbol to expr
    Rec(Vec<(Symbol, Box<Expr>, Expr)>) // bin symbol to expr (mutually) recursively
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
    pub fn new(name: String, disamb: u64) -> Self {
        Symbol{id: Id::new(name, disamb),
               left_assoc: false, priority: 0}
    }
    pub fn new_op(name: String, disamb: u64,
                  left_assoc: bool, priority: i8) -> Self {
        Symbol {
            id: Id::new(name, disamb),
            left_assoc, priority,
        }
    }
}

/**
 * There are primitives that aren't literals (like "blob")
 * and literals that aren't primitives (like "string")
 * primitive types can be operated on by primitive ops
 */
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub enum PrimitiveType {
    Bool, Int, Float, Char, Unit
}

impl fmt::Display for PrimitiveType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use PrimitiveType::*;
        match self {
            Bool => write!(f, "bool"), Int => write!(f, "int"),
            Float => write!(f, "float"), Char => write!(f, "char"),
            Unit => write!(f, "()")
        }
    }
}


#[derive(Clone, Debug)]
pub enum BaseType {
    Primitive(PrimitiveType),
    Tuple(Vec<Expr>),
    Record(Vec<(String, Expr)>),
    Variant(Vec<(String, Vec<Expr>)>),
    Module(Vec<(String, Expr)>)
}

/**
 * The desugared version of the language
 */
#[derive(Clone, Debug)]
pub enum Expr {
    // types are also expressions!
    Star, // * kind represents the "type" of a type
    Arrow(Box<Expr>, Box<Expr>), // type of lambda
    BaseType(BaseType), 
    Var(Id),
    Lit(Literal),

    Pack{tag: u16, args: Vec<Expr>, res_type: Box<Expr>},
    Case{expr: Box<Expr>, case_sym: Symbol, 
         alt: Vec<Alter>, res_type: Box<Expr>},
    Foreign{func: String, lam_type: Box<Expr>},

    Lam{var: Symbol, var_type: Box<Expr>, body: Box<Expr>},
    Let(Bind, Box<Expr>),
    App(Box<Expr>, Box<Expr>), // LHS is lambda, RHS is arg
    Builtin,
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
            _ => panic!("Unhandled ast type!")
        }
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