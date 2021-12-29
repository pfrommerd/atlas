use std::collections::HashMap;

pub type ExprBuilder<'a> = crate::core_capnp::expr::Builder<'a>;
pub type PrimitiveBuilder<'a> = crate::value_capnp::primitive::Builder<'a>;

// A symbol environment is for turning names into
// unique symbols that don't shadow each other
// TODO: Re-evaluate the need for the disambiguation ID
// under the new framework since we don't do typechecking
pub type DisambID = u32;
pub struct SymbolMap<'p> {
    parent: Option<&'p SymbolMap<'p>>,
    symbols: HashMap<String, DisambID>,
}

impl<'p> SymbolMap<'p> {
    pub fn new() -> Self {
        Self {
            parent: None,
            symbols: HashMap::new(),
        }
    }

    pub fn child(parent: &'p SymbolMap<'p>) -> Self {
        Self {
            parent: Some(parent),
            symbols: HashMap::new(),
        }
    }

    pub fn extend(&mut self, child: HashMap<String, DisambID>) {
        self.symbols.extend(child)
    }

    pub fn add(&mut self, name: &str) -> DisambID {
        let id = match self.lookup(name) {
            None => 0,
            Some(id) => id + 1
        };
        self.symbols.insert(String::from(name), id);
        id
    }

    pub fn lookup<'a>(&'a self, name: &str) -> Option<DisambID> {
        match self.symbols.get(name) {
            Some(s) => Some(*s),
            None => match self.parent {
                Some(parent) => parent.lookup(name),
                None => None,
            },
        }
    }
}