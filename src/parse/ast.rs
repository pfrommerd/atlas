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

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Literal {
    Bool(bool),
    Int(i64),
    Float(NotNan<f64>),
    String(String),
    Char(char)
}

// Type patterns are not like expression patterns!
// Type patterns match against types at compile time and are not lazily evaluated
// Only types can go in expression patterns i.e (A, int) = (float, int) the left hand pattern
// will match A to float

// Expression patterns are evaluated at run time and are for non-types i.e (0, x) = (0, 1) matches x = 1

// This is a type declaration, not a full type!
#[derive(Clone, Eq, Hash, Debug)]
pub enum TypeField<'src> {
    Simple(&'src str, Type<'src>),
    Expansion(Type<'src>)
}

#[derive(Clone, Eq, Hash, Debug)]
pub enum TypeComponent<'src> { // a tuple entry
    Simple(Type<'src>),
    Named(&'src str, Type<'src>)
}

#[derive(Clone, Eq, Hash, Debug)]
pub enum ArgType<'src> { // a tuple entry
    Postional(Type<'src>),              // int
    Named(&'src str, Type<'src>),       // ~foo:float
    Optional(&'src str, Type<'src>),    // ?foo:int
    VarPositional(Type<'src>),          // ..int list
    VarOptional(Type<'src>),            // ...int dict
}

#[derive(Clone, Eq, Hash, Debug)]
pub enum Type<'src> {
    Identifier(Span, &'src str),                           // A type identifier
    Generic(Span, &'src str),                              // 'a, 'b, i.e type generics
    Apply(Span, Vec<Type<'src>>, Box<Type<'src>>),         // int int tree or even 'a tree, 

    Project(Span, Box<Type<'src>>, &'src str),             // type.field (can be a record or a tuple!)

    Arrow(Span, Vec<ArgType<'src>>, Box<Type<'src>>),       // 'a -> 'b -> 'c

    Variant(Span, Vec<(&'src str, Vec<Type<'src>>)>),      // A int | B float float | C
    Tuple(Span, Vec<TypeComponent<'src>>),                   // (int, float, c: string) -- tuples are ordered even if labelled
    Record(Span, Vec<TypeField<'src>>),                      // { a : int, b : float, ..another type) -- records are not
    Pack(Span, Box<Type<'src>>, Box<Type<'src>>)           // type with types types
}

#[derive(Clone, Eq, Hash, Debug)]
pub enum TypePattern<'src> {
    Hole(Span),
    Identifier(Span, &'src str), // A type identifier
    Generic(Span, &'src str), // 'a
    Apply(Span, Vec<TypePattern<'src>>, Box<TypePattern<'src>>),

    Record(Span, Vec<(String, TypePattern<'src>)>),
    Tuple(Span, Vec<TypePattern<'src>>)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
// A type binding is a pattern on the lhs
// and a type on the rhs
// Note that this can result in multiple types potentially
pub struct TypeBindings<'src> {
    pub bindings: Vec<(TypePattern<'src>, Type<'src>)>
}

impl<'src> TypeBindings<'src> {
    pub fn new(b: Vec<(TypePattern<'src>, Type<'src>)>) -> Self {
        TypeBindings { bindings: b }
    }
}



// Expression-related structs


// Fields that come later override fields that come earlier
pub enum FieldExpr<'src> {
    Simple(String, Expr<'src>),
    Typed(String, Type<'src>, Expr<'src>),
    Expansion(Expr<'src>)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Expr<'src> {
    Identifier(Span, &'src str),
    Literal(Span, Literal),

    Unary(Span, &'src str, Vec<Expr<'src>>), // any operator that starts with a ! 
                                                 // like !$foo will be Unary(!$, foo)
    Infix(Span, Vec<Expr<'src>>, Vec<&'src str>), // 1 + 2 * 3 will be turned into Infix([1, 2, 3], [+, *]) and
                                                      // operator precedent/associativity will be
                                                      // determined in the parsing stage
    Apply(Span, Box<Expr<'src>>, Vec<Expr<'src>>),

    Macro(Span, &'src str, Vec<Expr<'src>>), // string! expr1 expr2 will be evaluated at module
                                             // instantiation time and can add dependencies to
                                             // the module expression
    // scoped let/type declarations

    // note that an expr binding (and an expr pattern) can also bind types
    // by using pack deconstructins (the types syntax!)

    LetIn(Span, LetBindings<'src>, Box<Expr<'src>>), 
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

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct LetBindings<'src> {
    pub bindings: Vec<(ExprPattern<'src>, Expr<'src>)>
}

impl<'src> LetBindings<'src> {
    pub fn new(b : Vec<(ExprPattern, Expr)>) -> Self {
        LetBindings { bindings: b }
    }
}

// A declaration is a top-level 
// type statement/let statement/export statement
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Declaration<'src> {
    // bool is whether this declaration is exported
    Type(Span, bool, TypeBindings<'src>),

    // bool is whether this declaration is exported
    Let(Span, bool, LetBindings<'src>), 

    // if we have a value export we can't have
    // any other export statements
    ValueExport(Span, Expr<'src>), 

    // if we have a binding export, we can't have
    // any other export statements
    BindingExport(Span, LetBindings<'src>)
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
