use crate::core::{Expr, Builtin, Literal};
use crate::value::{mem::MemoryAllocator, Numeric, Env};
use crate::optim::compile::Compile;
use crate::parse::{
    Lexer, ModuleParser,
    ast::Module
};

use super::tracer::ForceCache;
use super::machine::Machine;


use smol::LocalExecutor;
use futures_lite::future;

use test_log::test;

#[test]
fn test_core_add() {
    let add = 
        Expr::Builtin(Builtin { op :"add".to_string(), 
        args: vec![
            Expr::Literal(Literal::Int(42)),
            Expr::Literal(Literal::Int(24))
        ]});
    let alloc = MemoryAllocator::new();
    let env = Env::new();
    let thunk = add.compile(&alloc, &env).unwrap();

    let cache = ForceCache::new();
    let machine = Machine::new(&alloc, &cache);
    let exec = LocalExecutor::new();
    future::block_on(exec.run(async {
        let res = machine.force(thunk.clone()).await.unwrap();
        let val = res.as_numeric().unwrap();
        assert_eq!(val, Numeric::Int(66))
    }));
}

#[test]
fn test_prelude_end_to_end() {
    let alloc = MemoryAllocator::new();

    let prelude_lexed = Lexer::new(crate::core::prelude::PRELUDE);
    let prelude : Module = ModuleParser::new().parse(prelude_lexed).unwrap();
    // transpile the prelude
    let expr = prelude.transpile();
    let handle = expr.compile(&alloc, &Env::new()).unwrap();

    let cache = ForceCache::new();
    let machine = Machine::new(&alloc, &cache);
    let exec = LocalExecutor::new();
    future::block_on(exec.run(async {
        let res = machine.force(handle).await.unwrap();
        let val = res.as_record().unwrap();
        assert_eq!(val.len(), 4);
    }));
}