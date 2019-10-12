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

// Main AST Types

// Type patterns are not like expression patterns!
// Type patterns match against types at compile time and are not lazily evaluated
// Only types can go in expression patterns i.e (A, int) = (float, int) the left hand pattern
// will match A to float

// Expression patterns are evaluated at run time and are for non-types i.e (0, x) = (0, 1) matches x = 1

// This is a type declaration, not a full type!
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Type {
    Identifier(Span, String),             // A type identifier
    Generic(Span, String),                // 'a, 'b, i.e type generics
    Apply(Span, Vec<Type>, Box<Type>),    // int int tree or even 'a tree
    Arrow(Span, Box<Type>, Box<Type>),    // 'a -> 'b
    Tuple(Span, Vec<Type>),               // (int, float, string)
    Variant(Span, Vec<Type>),             // A | B | C
    Record(Span, BTreeMap<String, Type>)   // { a : int, b : float, etc. }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TypePattern {
    Hole(Span),
    Identifier(Span, String)
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

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Expr {
    Identifier(Span, String),
    Literal(Span, Literal),
    Infix(Span, Box<Expr>, Box<Expr>, Box<Expr>),
    Apply(Span, Box<Expr>, Vec<Box<Expr>>),

    Macro(Span, String, Box<Expr>), // string! expr for compile-time evaluation

    // scoped let/type declarations
    LetIn(Span, ExprBindings, Box<Expr>),
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
    values: ExprBindings
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
