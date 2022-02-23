pub use crate::store::op as op;
pub mod machine;
pub mod scope;
pub mod builtin;
pub mod tracer;

#[cfg(test)]
mod test;

pub use tracer::ForceCache;
pub use machine::Machine;

use crate::store::{Storage, Env};
use crate::compile::Compile;
use crate::error::Error;

use smol::LocalExecutor;
use futures_lite::future;

use std::collections::HashMap;
use std::collections::hash_map;
pub struct Env<'s, S: Storage> {
    map: HashMap<String, ObjHandle<'s, S>>
}

impl<'s, S: Storage> Env<'s, S> {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn insert(&mut self, key: String, value: ObjHandle<'s, S>) {
        self.map.insert(key, value);
    }

    pub fn iter<'m>(&'m self) -> hash_map::Iter<'m, String, ObjHandle<'s, S>> {
        self.map.iter()
    }
}

// TODO: Move into the cache?
pub fn populate_prelude<'a, S: Storage>(alloc: &'a A, env: &mut Env<'a, A>) -> Result<(), Error> {
    let prelude = crate::core::prelude::PRELUDE;
    let lexer = crate::parse::Lexer::new(prelude);
    let parser = crate::grammar::ModuleParser::new();
    let module : crate::parse::ast::Module = parser.parse(lexer).unwrap();
    let expr = module.transpile();
    let compiled = expr.compile(alloc, &Env::new())?;
    {
        // This is fine since we know evaluating the prelude
        // entries will not cause io operations
        let cache = ForceCache::new();
        let mach = Machine::new(alloc, &cache);
        let exec = LocalExecutor::new();
        future::block_on(exec.run(async {
            mach.env_use(compiled, env).await
        }))?;
    }
    Ok(())
}