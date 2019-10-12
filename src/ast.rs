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
    Byte(u8),
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
    Any,                            // _
    Identifier(String),             // A type identifier
    Var(String),                    // 'a, 'b, i.e type variables
    Apply(Vec<Type>, Box<Type>),    // fills in the types as in int list
    Arrow(Box<Type>, Box<Type>),    // 'a -> 'b
    Tuple(Vec<Type>),               // (int, float, string)
    Variant(Vec<Type>),             // A | B | C
    Record(BTreeMap<String, Type>)   // { a : int, b : float, etc. }
}


#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TypePattern {
    Any,
    Identifier(String)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
// A type binding is a pattern on the lhs
// and a type on the rhs
// Note that this can result in multiple types potentially
pub struct TypeBinding {
    pattern: TypePattern,
    expression: Type
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Expr {
    Identifier(Span, String),
    Literal(Span, Literal),
    Infix(Span, Box<Expr>, String, Box<Expr>),
    Apply(Span, Box<Expr>, Vec<Box<Expr>>),

    Macro(Span, String, Box<Expr>), // string! expr for compile-time evaluation

    // scoped let/type declarations
    LetIn(Span, Vec<ExprBinding>, Box<Expr>),
    TypeIn(Span, Vec<TypeBinding>, Box<Expr>),

    IfElse {
        span: Span, 
        condition: Box<Expr>, 
        success: Box<Expr>, 
        failure: Box<Expr>
    }, // condition, true, false

    Project(Span, Box<Expr>, String), // record.field syntax (or tuple.x equivalently)

    Match(Span, Box<Expr>, Vec<(ExprPattern, Expr)>), // match with syntax, note that the tuples are not 

    Fun(Span, Vec<ExprPattern>, Box<Expr>)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ExprPattern {
    Identifier(Span, String)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ExprBinding {
    pattern: ExprPattern,
    expression: Expr
}

// A declaration is a top-level 
// type statement/let statement/export statement
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Declaration {
    Type(Vec<TypeBinding>),
    Let(Vec<ExprBinding>),
    ExportLet(Vec<ExprBinding>), // export + let statement combined
    ExportPattern(Vec<ExprPattern>), // does the export but not the let
    ExportValue(Expr) // if we have a value export, we can't export anything else!
}

pub struct Module {
}

//
