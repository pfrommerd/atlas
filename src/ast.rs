use ordered_float::NotNan;
use std::collections::HashMap;

pub use codespan::{
    ByteIndex,
    ColumnIndex,
    LineIndex
};

#[derive(Copy, Clone, Default, Eq, PartialEq, Debug, 
         Hash, Ord, PartialOrd)]
pub struct Location {
    pub line: LineIndex,
    pub col: ColumnIndex,
    pub abs: ByteIndex
}

impl Location {
    pub fn shift(&mut self, c: isize) {
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Literal {
    Byte(u8),
    Bool(bool),
    Int(i64),
    Float(NotNan<f64>),
    String(String),
    Char(char)
}

// This is a type declaration, not a full type!
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Type {
    Any,                            // _
    Ident(String),                  // A type identifier
    Var(String),                    // 'a, 'b, i.e type variables
    Arrow(Box<Type>, Box<Type>),    // 'a -> 'b
    Tuple(Vec<Type>),               // (int, float, string)
    Variant(Vec<Type>),             // A | B | C
    Record(HashMap<String, Type>)   // { a : int, b : float }
}

// Type patterns are not like expression patterns!
// Type patterns match against types at compile time and are not lazily evaluated
// Only types can go in expression patterns i.e (A, int) = (float, int) the left hand pattern
// will match A to float

// Expression patterns are evaluated at run time and are for non-types i.e (0, x) = (0, 1) matches x = 1

pub enum TypePattern {
}

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

    // scoped let/type declarations
    LetIn(Span, Vec<(ExprPattern, Expr)>, Box<Expr>),
    TypeIn(Span, Vec<(TypePattern, Type)>, Box<Expr>),

    IfElse {
        span: Span, 
        condition: Box<Expr>, 
        success: Box<Expr>, 
        failure: Box<Expr>
    }, // condition, true, false

    Project(Span, Box<Expr>, String), // record.field syntax (or tuple.x equivalently)

    Match(Span, Box<Expr>, Vec<(ExprPattern, Expr)>), // match with syntax

    Fun(Span, Vec<ExprPattern>, Box<Expr>)
}

pub enum ExprPattern {
}

pub struct ExprBinding {
    pattern: ExprPattern,
    expression: Expr
}

// A declaration is a top-level 
// type statement/let statement/export statement
pub enum Declaration {
    Type(Vec<TypePattern,Type>),
    Let(Vec<(Pattern,Expr)>),
    ExportLet(Vec<(Pattern, Expr)>), // export + let statement combined
    ExportPattern(Vec<(Pattern, Expr)>), // does the export but not the let
    ExportValue(Expr)
}

//
