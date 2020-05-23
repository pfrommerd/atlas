use ordered_float::NotNan;

pub use codespan::{
    ByteIndex,
    ColumnIndex,
    LineIndex,
    ColumnOffset,
    LineOffset,
    ByteOffset,
    Span
};

// Type patterns are not like expression patterns!
// Type patterns match against types at compile time and are not lazily evaluated
// Only types can go in expression patterns i.e (A, int) = (float, int) the left hand pattern
// will match A to float

// Expression patterns are evaluated at run time and are for non-types i.e (0, x) = (0, 1) matches x = 1

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum FieldType<'src> {
    Default(Span, &'src str), // {Bar} (equivalent to {Bar: Bar})
    Simple(Span, &'src str, Span, Type<'src>), // a : int
    Expansion(Span, Type<'src>) // ...another_type
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ArgType<'src> { // a tuple entry
    Positional(Type<'src>),              // int
    Named(Span, &'src str, Span, Type<'src>),       // ~foo:float
    VariablePositional(Span, Type<'src>),          // ..int list

    Optional(Span, &'src str, Span, Type<'src>),    // ?foo:int
    VariableOptional(Span, Type<'src>),            // ...int dict
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Type<'src> {
    Hole(Span),                                            // cannot end up in a concrete type!
    Identifier(Span, &'src str),                           // A type identifier
    Applied(Span, Vec<Type<'src>>, Box<Type<'src>>),         // int int tree or even 'a tree, 

    Project(Span, Box<Type<'src>>, &'src str),              // type.field

    Arrow(Span, Vec<ArgType<'src>>, Box<Type<'src>>),            // 'a -> 'b -> 'c

    Variant(Span, Vec<(&'src str, Type<'src>)>),      // A int | B (float,float) | C
    Tuple(Span, Vec<Type<'src>>),                   // (int, float, string) 
    Record(Span, Vec<FieldType<'src>>),                      // { a : int, b : float, ..another type }

    // shorthands like [int] instead of int list
    // List(Span, Box<Type<'src>>)

    Error()
}

// A type binding is a type (used like a pattern) on the lhs
// and a type on the rhs
// Note that this can result in multiple types potentially
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TypeBindings<'src> {
    pub bindings: Vec<(Type<'src>, Type<'src>)>
}

impl<'src> TypeBindings<'src> {
    pub fn new(b: Vec<(Type<'src>, Type<'src>)>) -> Self {
        TypeBindings { bindings: b }
    }
}

// Expression-related structs

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Literal {
    Bool(bool),
    Int(i64),
    Float(NotNan<f64>),
    String(String),
    Char(char)
}

// Fields that come later override fields that come earlier
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum FieldExpr<'src> {
    Default(Span, &'src str),
    Simple(Span, &'src str, Span, Expr<'src>), // let c = { a = 0, b = 1}
    Expansion(Span, Expr<'src>) // let c = { a = 0, ...b }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Expr<'src> {
    Identifier(Span, &'src str),
    Literal(Span, Literal),
    Constraint(Box<Expr<'src>>, Type<'src>), // type constraint

    List(Span, Vec<Expr<'src>>), // list literal [a; b; c; d]
    Record(Span, Vec<FieldExpr<'src>>), // record literal { a = 1, b = 2 }

    Prefix(Span, &'src str, Box<Expr<'src>>),     // any operator that starts with a !
                                                 // like !$foo will be Unary(!$, foo)
    Infix(Span, Vec<(Expr<'src>, &'src str)>, Box<Expr<'src>>), // 1 + 2 * 3 will be turned into Infix([1, 2, 3], [+, *]) and
                                                  // operator precedent/associativity will be
                                                  // determined in the parsing stage
    App(Span, Box<Expr<'src>>, Vec<Expr<'src>>),

    Macro(Span, &'src str, Vec<Expr<'src>>), // string! expr1 expr2 will be evaluated at module
                                             // instantiation time and can add dependencies to
                                             // the module expression

    // scoped let/type declarations
    LetIn(Span, ExprBindings<'src>, Box<Expr<'src>>), 
    TypeIn(Span, TypeBindings<'src>, Box<Expr<'src>>),

    IfElse(Span, Box<Expr<'src>>, Box<Expr<'src>>, Box<Expr<'src>>),

    Project(Span, Box<Expr<'src>>, &'src str),

    // match with syntax, note that the tuples are not 
    Match(Span, Box<Expr<'src>>, Vec<(ExprPattern<'src>, Expr<'src>)>), 

    Fun(Span, Vec<ExprPattern<'src>>, Box<Expr<'src>>)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ExprPattern<'src> {
    Hole(Span), // _
    Identifier(Span, &'src str)
}


// various and'ed bindings
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ExprBindings<'src> {
    pub bindings: Vec<(ExprPattern<'src>, Expr<'src>)>
}

// various and'ed bindings
impl<'src> ExprBindings<'src> {
    pub fn new(b : Vec<(ExprPattern<'src>, Expr<'src>)>) -> Self {
        ExprBindings{ bindings: b }
    }
}

// A declaration is a top-level 
// type statement/let statement/export statement
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Declaration<'src> {
    // bool is whether this declaration is exported
    TypeDeclare(Span, bool, TypeBindings<'src>),

    // bool is whether this declaration is exported
    LetDeclare(Span, bool, ExprBindings<'src>), 
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ReplInput<'src> {
    Decl(Declaration<'src>),
    Expr(Expr<'src>),
    Type(Type<'src>)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Module<'src> {
    pub declarations: Vec<Declaration<'src>>
}

impl<'src> Module<'src> {
    pub fn new(declarations: Vec<Declaration<'src>>) -> Self {
        Module{declarations: declarations}
    }
}