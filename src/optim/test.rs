use crate::core::{Expr, Builtin, Literal};
use crate::value::mem::MemoryAllocator;
use crate::optim::Env;
use crate::optim::compile::Compile;


#[test]
fn test_add() {
    let add =
        Expr::Builtin(Builtin { op :"add".to_string(), 
        args: vec![
            Expr::Literal(Literal::Int(1)),
            Expr::Literal(Literal::Int(1))
        ]});
    let alloc = MemoryAllocator::new();
    let env = Env::new();
    let _thunk = add.compile(&alloc, &env).unwrap();
}