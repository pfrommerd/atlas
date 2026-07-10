mod decl;
mod expr;
mod types;

pub use decl::*;
pub use expr::*;
pub use types::*;

#[derive(Debug, Clone)]
pub enum ReplInput<'src> {
    Expr(Expr<'src>),
    Declaration(Declaration<'src>),
}

#[derive(Debug, Clone)]
pub enum Input<'src> {
    Repl(ReplInput<'src>),
    Expr(Expr<'src>),
    Module(Module<'src>),
}
