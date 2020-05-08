use ordered_float::NotNan;

use std::sync::Arc;

// The core-language definition, very similar to
// that used by GHC. Extended slightly to support the extra features
// we have for adding dependencies. All types are represented as tuples
// but it preserves interface information.

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum Literal {
    Bool(bool),
    Int(i64),
    Float(NotNan<f64>),
    String(String),
    Char(char)
}

#[derive(Clone, Debug)]
pub enum Bind {
    NonRec(Symbol, Box<Expr>), // bind symbol to expr
    Rec(Vec<(Symbol, Expr)>) // bin symbol to expr (mutually) recursively
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub struct Symbol {
    // contains optional debugging information
    name: Option<String>, // the variable name
    disamb: u64 // used to disambugate shadowed parameters, 
                // incremented for every shadow
}

// id equality does not do type equality!
// if two symbols are the same but their types
// are different something is very wrong!
#[derive(Clone, Debug)]
pub struct Id {
    sym: Symbol,
    sym_type: Box<Expr>
}

// re-sugaring information

// Contains re-sugaring information about a pack
// i.e was it a record, tuple, variant, variant type, module, 
// the names and positions into which fields were desuraged, etc.
#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum PackInfo {
    Tuple,
    Record(Vec<String>) // names corresponding to the slots of the pack
}

#[derive(Clone, Eq, PartialEq, Hash, Debug)]
pub enum PrimitiveType {
    Bool, Int, Float, String, Char
}

/**
 * The desugared version of the language
 */
#[derive(Clone, Debug)]
pub enum Expr {
    // types are also expressions!
    Star, // * kind represents the "type" of a concrete data type
    Arrow{left: Box<Expr>, right: Box<Expr>}, // for type expression

    PrimType(PrimitiveType),
    // all data structures are tuples,
    // info contains information on how the associated type was desuraged
    // so it can be "reinflated"
    Pack{tag: u16, arg_types: Vec<Expr>, info: Arc<PackInfo>}, 

    Var(Id), Type(Id),

    // construct a pack type with a tag and a bunch of types
    // the type of this is an arrow between all the types with
    // a return type of a "Pack" if args > 0 and directly a pack if no args
    // info contains the desugaring information about the type that is being constructed
    Constr{tag: u16, arg_types: Vec<Expr>, info: Arc<PackInfo>},
    Case{expr: Box<Expr>, case_sym: Symbol, 
         alt: Vec<Alter>, res_type: Box<Expr>},
    // extract ith element from pack, panic on failure
    // useful shorthand for tuple/record indexing
    Extract{expr: Box<Expr>, i: u16, extract_type: Box<Expr>},
    Foreign{func: String, lam_type: Box<Expr>}, // Foreign lambda function

    Lit(Literal),
    Lam(Id, Box<Expr>),
    Let(Bind, Box<Expr>),
    App(Box<Expr>, Box<Expr>), // LHS is lambda, RHS is arg
    Cast(Box<Expr>, Box<Expr>), // operator used to cast between types
    TypeOf(Box<Expr>), // operator used to get the type of a core expression
                       // as an expression
                       // used only for debugging and type-checking
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
    pub fn type_expr(&self) -> Expr {
        use Expr::*;

        match self {
            Star => Star,
            Arrow{left:_, right:_} => Star,
            Pack{tag:_, arg_types:_, info:_} => Star,
            PrimType(_) => Star,
            Var(Id{sym:_, sym_type}) => (**sym_type).clone(),
            Type(Id{sym:_, sym_type}) => (**sym_type).clone(),
            Constr{tag, arg_types, info} => {
                if arg_types.len() == 0 {
                    Pack{tag: *tag, arg_types: vec![], info: info.clone()}
                } else {
                    let mut i = arg_types.iter().rev();
                    let mut x = Arrow{left: Box::new(i.next().unwrap().clone()),
                                      right: Box::new(Pack{tag: *tag, 
                                                arg_types: arg_types.clone(),
                                                info: info.clone()})};
                    for a in i {
                        x = Arrow{left: Box::new(a.clone()), right: Box::new(x)};
                    }
                    x
                }
            },
            Lam(Id{sym:_, sym_type}, body) => 
                Arrow{left: sym_type.clone(), right: Box::new(body.type_expr())},
            Foreign{func:_, lam_type} => (**lam_type).clone(),
            Case{expr:_, case_sym:_, alt:_, res_type} => (**res_type).clone(),
            Extract{expr:_, i:_, extract_type} => (**extract_type).clone(),
            Lit(value) => {
                use PrimitiveType::*;
                match value {
                    Literal::Bool(_) => 
                        Expr::PrimType(Bool),
                    Literal::Int(_) => 
                        Expr::PrimType(Int),
                    Literal::Float(_) => Expr::PrimType(Float),
                    Literal::String(_) => Expr::PrimType(String),
                    Literal::Char(_) => Expr::PrimType(Char)
                }
            }
            Let(_bind, body) => body.type_expr(),
            App(_lambda, _arg) => Bad,
            Cast(_exp, coercion) => *coercion.clone(),
            TypeOf(_exp) => Star,
            Bad => Bad
        }
    }
}