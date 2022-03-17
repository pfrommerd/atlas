use crate::core::{Expr, Builtin, Literal};
use crate::store::heap::HeapStorage;
use crate::compile::CodeGraph;
use super::Env;
use crate::compile::{Compile, CompileEnv};

#[test]
fn test_add_graph() {
    let add =
        Expr::Builtin(Builtin { op :"add".to_string(), 
        args: vec![
            Expr::Literal(Literal::Int(1)),
            Expr::Literal(Literal::Int(1))
        ]});
    let alloc = HeapStorage::new();
    let cenv = CompileEnv::new();
    let mut graph = CodeGraph::new();
    let _thunk = add.compile_with(&alloc, &cenv, &mut graph);
    println!("graph: {:?}", graph);
    println!("res: {:?}", _thunk);
}

#[test]
fn test_add_packed() {
    let add =
        Expr::Builtin(Builtin { op :"add".to_string(), 
        args: vec![
            Expr::Literal(Literal::Int(1)),
            Expr::Literal(Literal::Int(1))
        ]});
    let s = HeapStorage::new();
    let env = Env::new();
    let code = add.compile(&s, &env).unwrap().to_code(&s).unwrap();
    std::mem::drop(code)
    // println!("code: {}", code.reader());
}