use ordered_float::NotNan;


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
    String(String, Box<Expr>), 
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
// the names and positions into which fields were desuraged, 
// and any associated types
#[derive(Clone, Debug)]
pub enum DataInfo {
    Tuple(Vec<Expr>),
    Record(Vec<(String, Expr)>),
    Variant(Vec<(String, Vec<Expr>)>),
    Module(Vec<(String, Expr)>) // how the exports are packed
}

/**
 * There are primitives that aren't literals (like "blob")
 * and literals that aren't primitives (like "string")
 * primitive types can be operated on by primitive ops
 */
#[derive(Clone, Eq, PartialEq, Hash, Debug, Trace, Finalize)]
pub enum PrimitiveType {
    Bool, Int, Float, Char, Unit
}

#[derive(Clone, Eq, PartialEq, Hash, Debug, Trace, Finalize)]
pub enum PrimitiveOp {
    BNegate, // flip boolean type
    IAdd, ISub, IMul, IDiv, IMod, INegate, // integer operations 
    FAdd, FSub, FMul, FDiv, FNegate, // float operations
}

/**
 * The desugared version of the language
 */
#[derive(Clone, Debug)]
pub enum Expr {
    // types are also expressions!
    Star, // * kind represents the "type" of a concrete data type
    Arrow(Box<Expr>, Box<Expr>), // for type expression

    Var(Id), Type(Id),

    PrimType(PrimitiveType),
    PrimOp(PrimitiveOp),

    // information about a particular pack constructor,
    // DataInfo contains type re-sugaring information
    DataType(DataInfo), 

    // type_info contains the type information
    Pack{tag: u16, arg_types: Vec<Expr>, type_info: Box<Expr>},
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