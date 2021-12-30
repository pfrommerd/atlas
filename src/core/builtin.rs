use super::lang::{SymbolMap};

pub fn symbols() -> SymbolMap<'static> {
    let mut env = SymbolMap::new();
    env.add("*");
    env.add("/");
    env.add("+");
    env.add("-");
    env
}
