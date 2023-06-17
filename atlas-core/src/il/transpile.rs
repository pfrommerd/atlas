use atlas_parse::ast::{
    Expr as AstExpr,
    Literal as AstLiteral,
};

use super::{Expr, Constant};

pub trait Transpile {
    fn transpile(&self) -> Expr;
}

impl Transpile for AstExpr<'_> {
    fn transpile(&self) -> Expr {
        todo!()
    }
}

impl Transpile for AstLiteral<'_> {
    fn transpile(&self) -> Expr {
        use AstLiteral::*;
        Expr::Const(match self {
            &Integer(i) => Constant::Integer(i),
            Float(f) => Constant::Float(f.into_inner()),
            &Bool(b) => Constant::Bool(b),
            String(s) => Constant::String(s.to_string()),
            Unit => Constant::Unit
        })
    }
}