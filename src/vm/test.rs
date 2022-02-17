use crate::core::{Expr, Builtin, Literal};
use crate::value::{mem::MemoryAllocator, Numeric};
use crate::optim::Env;
use crate::optim::compile::Compile;

use super::tracer::ForceCache;
use super::machine::Machine;

use smol::LocalExecutor;
use futures_lite::future;


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
    let thunk = add.compile(&alloc, &env).unwrap();
    let cache = ForceCache::new();

    let machine = Machine::new(&alloc, &cache);
    let exec = LocalExecutor::new();
    future::block_on(exec.run(async {
        let res = machine.force(thunk.clone()).await.unwrap();
        let val = res.as_numeric().unwrap();
        assert_eq!(val, Numeric::Int(3))
    }));
}