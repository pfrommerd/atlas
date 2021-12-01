use super::lang::{Symbol, SymbolMap};

pub fn symbols() -> SymbolMap<'static> {
    let mut env = SymbolMap::new();
    env.add(Symbol::new(String::from("*"), 0));
    env.add(Symbol::new(String::from("/"), 0));
    env.add(Symbol::new(String::from("+"), 0));
    env.add(Symbol::new(String::from("-"), 0));
    env
}
