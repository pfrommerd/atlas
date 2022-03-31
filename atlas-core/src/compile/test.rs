use crate::core::{Expr, Builtin, Literal};
use crate::store::heap::HeapStorage;
use crate::store::{Storable, Storage, CodeReader};
use crate::compile::CodeGraph;
use crate::compile::{Compile, CompileEnv};
use super::Env;
use crate::store::print::Depth;

use pretty::{BoxDoc, BoxAllocator};


#[test]
fn test_compile_add() {
    let add =
        Expr::Builtin(Builtin { op :"add".to_string(), 
        args: vec![
            Expr::Literal(Literal::Int(1)),
            Expr::Literal(Literal::Int(2))
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

use crate::store::value::Value;

#[test]
fn test_compile_prelude() {
    let storage = HeapStorage::new();
    let mut env = Env::new();

    let prelude_src = crate::core::prelude::PRELUDE;
    let mut dir_path = "file:///test".to_owned();
    let dir_path = storage.insert_from(&Value::String(dir_path)).unwrap();
    env.insert(String::from("__path__"), dir_path);
    let lexer = crate::parse::Lexer::new(prelude_src);
    let parser = crate::grammar::ModuleParser::new();
    let module : crate::parse::ast::Module = parser.parse(lexer).unwrap();
    let expr = module.transpile();
    let prelude_compiled = expr.compile(&storage, &env).unwrap().to_code(&storage).unwrap();
    {
        let code_reader = prelude_compiled.reader();
        let code_doc: BoxDoc<'_, ()> = code_reader.pretty(Depth::Fixed(2), &BoxAllocator).into_doc();
        println!("code: {}", code_doc.pretty(80));
    }
    todo!()
}