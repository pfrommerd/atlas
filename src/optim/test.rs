use crate::core::{Expr, Builtin, Literal};
use crate::value::mem::MemoryAllocator;
use crate::optim::graph::CodeGraph;
use crate::optim::Env;
use crate::optim::compile::{Compile, CompileEnv};

#[test]
fn test_add_graph() {
    let add =
        Expr::Builtin(Builtin { op :"add".to_string(), 
        args: vec![
            Expr::Literal(Literal::Int(1)),
            Expr::Literal(Literal::Int(1))
        ]});
    let alloc = MemoryAllocator::new();
    let cenv = CompileEnv::new();
    let graph = CodeGraph::new();
    let _thunk = add.compile_into(&alloc, &cenv, &graph).unwrap();
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
    let alloc = MemoryAllocator::new();
    let env = Env::new();
    let entry = add.compile(&alloc, &env).unwrap();
    let thunk_target = entry.as_thunk().unwrap();
    let code = thunk_target.as_code().unwrap();
    println!("code: {}", code.reader());
}