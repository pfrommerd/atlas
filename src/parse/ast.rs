use ordered_float::NotNan;
use std::collections::BTreeMap;

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
pub enum TypeField<'input> {
    Simple(&'input str, Type),
    ExpansionWith(Type, Vec<TypeField>), // ..type with a : int, b : float, etc..
    Expansion(Type)
}

#[derive(Clone, Eq, Hash, Debug)]
pub enum TypeEntry<'input> { // an tuple entry
    Simple(Type),
    Named(&'input str, Type)
}

#[derive(Clone, Eq, Hash, Debug)]
pub enum Type<'input> {
    Identifier(Span, &'input str),                             // A type identifier
    Generic(Span, &'input str),                                // 'a, 'b, i.e type generics
    Apply(Span, Vec<Type<'input>>, Box<Type<'input>>),         // int int tree or even 'a tree, 

    Project(Span, Box<Type<'input>>, &'input str),             // type.field (can be a record or a tuple!)

    Arrow(Span, Box<Type<'input>>, Box<Type<'input>>),         // 'a -> 'b

    Variant(Span, Vec<(&'input str, Vec<Type<'input>>)>),      // A int | B float float | C
    Tuple(Span, Vec<TypeEntry>),                               // (int, float, c: string) -- tuples are ordered even if labelled
    Record(Span, Vec<TypeField>),                              // { a : int, b : float, ..another type) -- records are not
    Pack(Span, Box<Type>, Box<Type>)                           // type with types types
}

#[derive(Clone, Eq, Hash, Debug)]
pub enum TypePattern {
    Hole(Span),
    Identifier(Span, String),
    Generic(Span, String),
    Apply(Span, Vec<TypePattern>, Box<TypePattern>),

    Record(Span, Vec<(String, TypePattern)>),
    Tuple(Span, Vec<TypePattern>)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
// A type binding is a pattern on the lhs
// and a type on the rhs
// Note that this can result in multiple types potentially
pub struct TypeBindings {
    pub bindings: Vec<(TypePattern, Type)>
}

impl TypeBindings {
    pub fn new(b: Vec<(TypePattern, Type)>) -> Self {
        TypeBindings { bindings: b }
    }
}

// Note that one field expression does not necessarily
// correspond to one field in the case of expansions
pub enum FieldExpr {
    Simple(String, Expr),
    Typed(String, TypeExpr, Expr),
    Expansion(Expr)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Expr<'input> {
    Identifier(Span, &'input str),
    Literal(Span, Literal),

    Unary(Span, &'input str, Vec<Expr<'input>>), // any operator that starts with a ! 
                                                 // like !$foo will be Unary(!$, foo)
    Infix(Span, Vec<Expr<'input>>, Vec<&'input str>), // 1 + 2 * 3 will be turned into Infix([1, 2, 3], [+, *]) and
                                                      // operator precedent/associativity will be
                                                      // determined in the parsing stage
    Apply(Span, Box<Expr<'input>>, Vec<Expr<'input>>),

    Macro(Span, &'input str, Vec<Expr>), // string! expr1 expr2 will be evaluated at compile-time
                                         // using a macro definition which (for now) can only be
                                         // implemented in Rust. Once better rust-atlas type
                                         // binding macros are done the idea is to expose this ast
                                         // to atlas and let atlas-defined macros exist
    // scoped let/type declarations
    LetIn(Span, ExprBindings, Box<Expr>), // note that an expr binding (and an expr pattern) can also bind types
                                          // by using packs (with types syntax!)
    TypeIn(Span, TypeBindings, Box<Expr>),

    IfElse(Span, Box<Expr>, Box<Expr>, Box<Expr>),

    Project(Span, Box<Expr>, String), // record.field syntax (or tuple.x equivalently)

    Match(Span, Box<Expr>, Vec<(ExprPattern, Expr)>), // match with syntax, note that the tuples are not 

    Fun(Span, Vec<ExprPattern>, Box<Expr>)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ExprPattern {
    Hole(Span), // _
    Identifier(Span, String)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ExprBindings {
    pub bindings: Vec<(ExprPattern, Expr)>
}

impl ExprBindings {
    pub fn new(b : Vec<(ExprPattern, Expr)>) -> Self {
        ExprBindings { bindings: b }
    }
}

// A declaration is a top-level 
// type statement/let statement/export statement
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Declaration {
    span: Span,
    exported: bool,
    types: TypeBindings,
    values: ExprBindings,
    Type(Span, bool, TypeBindings), // bool is whether this is exported
    Let(Span, bool, ExprBindings), // bool is whether this is exported
    ValueExport(Span, Expr), // if we have a value export, we can't export any other values
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct File {
    pub declarations: Vec<Declaration>
}

impl File {
    pub fn new(declarations: Vec<Declaration>) -> Self {
        File{declarations: declarations}
    }
}
