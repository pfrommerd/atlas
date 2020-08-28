use ordered_float::NotNan;
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
pub struct Symbol {
    // contains optional debugging information
    name: String, // the variable name
    disamb: u64 // used to disambugate shadowed parameters, 
                // incremented for every shadow
}

impl Symbol {
    pub fn new(s: String, disamb: u64) -> Self {
        Symbol{name: s, disamb: disamb}
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
    Var(Symbol),
    Lit(Literal),

    Pack{tag: u16, args: Vec<Expr>, res_type: Box<Expr>},
    Case{expr: Box<Expr>, case_sym: Symbol, 
         alt: Vec<Alter>, res_type: Box<Expr>},
    Foreign{func: String, lam_type: Box<Expr>},

    Lam{var: Symbol, var_type: Box<Expr>, body: Box<Expr>},
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
    pub fn compile_expr(ast: &AstExpr) -> Expr {
        match ast {
            AstExpr::Literal(_, lit) => {
                Expr::Lit(Literal::from_ast(lit.clone()))
            },
            AstExpr::Infix(_, args, ops) => {
                // find splitting op
                if args.len() == 1 { 
                    Expr::compile_expr(&args[0])
                } else {
                    // split by lowest-precedence operation
                    let split_idx = 0;
                    let mut largs = args.clone();
                    let rargs = largs.split_off(split_idx + 1);

                    let mut lops = ops.clone();
                    let mut cops = lops.split_off(split_idx);
                    let rops = cops.split_off(1);

                    let op = cops[0];
                    let left = AstExpr::Infix(Span::new(0, 0), largs, lops);
                    let right = AstExpr::Infix(Span::new(0, 0), rargs, rops);

                    let sym = Symbol::new(String::from(op), 0);
                    Expr::App(Box::new(Expr::App(
                        Box::new(Expr::Var(sym)), 
                        Box::new(Expr::compile_expr(&left))
                    )), Box::new(Expr::compile_expr(&right)))
                }
            },
            _ => panic!("Unhandled ast type!")
        }
    }
}

/*
impl Expr {
    pub fn type_expr(&self) -> Expr {
        use Expr::*;

        match self {
            Star => Star,
            Arrow(_l, _r) => Star,
            Data(_) => Star,
            PrimType(_) => Star,
            PrimOp(op) => {
                use PrimitiveOp::*;
                use PrimitiveType::*;
                let iop = Arrow(
                    Box::new(PrimType(Int)),
                    Box::new(Arrow(Box::new(PrimType(Int)), Box::new(PrimType(Int))))
                );
                let fop = Arrow(
                    Box::new(PrimType(Float)),
                    Box::new(Arrow(Box::new(PrimType(Float)), Box::new(PrimType(Float))))
                );
                match op {
                BNegate => Arrow(Box::new(PrimType(Bool)), Box::new(PrimType(Bool))),
                IAdd => iop,
                ISub => iop,
                IMul => iop,
                IDiv => iop,
                IMod => iop,
                INegate => Arrow(Box::new(PrimType(Int)), Box::new(PrimType(Int))),
                FAdd => fop,
                FSub => fop,
                FMul => fop,
                FDiv => fop,
                FNegate => Arrow(Box::new(PrimType(Float)), Box::new(PrimType(Float)))
                }
            },
            Var(Id{sym:_, sym_type}) => (**sym_type).clone(),
            Type(Id{sym:_, sym_type}) => (**sym_type).clone(),
            Constr{tag, arg_types, info} => {
                if arg_types.len() == 0 {
                    Pack{tag: *tag, arg_types: vec![], info: info.clone()}
                } else {
                    let mut i = arg_types.iter().rev();
                    let mut x = Arrow(Box::new(i.next().unwrap().clone()),
                                      Box::new(Pack{tag: *tag, 
                                                arg_types: arg_types.clone(),
                                                info: info.clone()}));
                    for a in i {
                        x = Arrow(Box::new(a.clone()), Box::new(x));
                    }
                    x
                }
            },
            Lam(Id{sym:_, sym_type}, body) => 
                Arrow(sym_type.clone(), Box::new(body.type_expr())),
            Foreign{func:_, lam_type} => (**lam_type).clone(),
            Case{expr:_, case_sym:_, alt:_, res_type} => (**res_type).clone(),
            Extract{expr:_, i:_, extract_type} => (**extract_type).clone(),
            Lit(value) => {
                match value {
                    Literal::Bool(_) => PrimType(PrimitiveType::Bool),
                    Literal::Int(_) => PrimType(PrimitiveType::Int),
                    Literal::Float(_) => PrimType(PrimitiveType::Bool),
                    Literal::Char(_) =>PrimType(PrimitiveType::Char),
                    Literal::String(_, x) => (**x).clone()
                }
            }
            Let(_bind, body) => body.type_expr(),
            App(_lambda, _arg) => Bad,
            TypeOf(_exp) => Star,
            Bad => Bad
        }
    }
}
        */