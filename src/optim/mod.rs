use std::collections::HashMap;

use crate::value::Storage;
use crate::core::lang::SymbolMap;

pub mod graph;
pub mod transpile;
pub mod compile;


pub struct Env<'s, S: Storage + 's> {
    pub map: HashMap<String, S::ObjectRef<'s>>
}

impl<'s, S: Storage + 's> Env<'s, S> {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    pub fn insert(&mut self, key: String, value: S::ObjectRef<'s>) {
        self.map.insert(key, value);
    }

    pub fn symbol_map(&self) -> SymbolMap<'static> {
        let mut s = SymbolMap::new();
        for sym in self.map.keys() {
            s.add(sym);
        }
        s
    }
}