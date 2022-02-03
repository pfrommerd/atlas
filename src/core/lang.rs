use std::collections::{HashMap, HashSet};

use super::util::PrettyReader;
use pretty::{DocAllocator, DocBuilder};

pub use crate::core_capnp::expr::{
    Which as ExprWhich,
    Builder as ExprBuilder,
    Reader as ExprReader,
};
pub use crate::core_capnp::case::{
    Which as CaseWhich,
    Builder as CaseBuilder,
    Reader as CaseReader,
};
pub use crate::value_capnp::primitive::{
    Which as PrimitiveWhich,
    Builder as PrimitiveBuilder,
    Reader as PrimitiveReader
};
pub use crate::core_capnp::expr::apply::{
    Which as ApplyWhich,
    Builder as ApplyBuilder,
    Reader as ApplyReader
};
pub use crate::core_capnp::expr::param::{
    Which as ParamWhich,
    Builder as ParamBuilder,
    Reader as ParamReader
};
pub use crate::core_capnp::symbol::{
    Builder as SymbolBuilder,
    Reader as SymbolReader
};
pub use crate::core_capnp::expr::binds::{
    Builder as BindsBuilder,
    Reader as BindsReader
};
pub use crate::core_capnp::expr::binds::bind::{
    Builder as BindBuilder,
    Reader as BindReader
};

impl<'s> ExprReader<'s> {
    // TODO: We call this recursively on every node,
    // which in turn computes the free variables recursively on every node
    // this is quite inefficient, and could potentially be done better
    // by using dynamic programming/a hashmap to store intermediate results
    // to get the free variables for everything in one pass
    pub fn free_variables<'a>(&'a self, bound: &HashSet<(&'s str, DisambID)>) -> HashSet<(&'s str, DisambID)> {
        use ExprWhich::*;
        match self.which().unwrap() {
            Id(r) => {
                let r = r.unwrap();
                let mut hs = HashSet::new();
                hs.insert((r.get_name().unwrap(), r.get_disam()));
                hs
            },
            Literal(_) => HashSet::new(),
            App(a) => {
                let mut hs = HashSet::new();
                for s in a.clone().get_args().unwrap()
                        .iter().map(|x| x.get_value().unwrap().free_variables(bound)) {
                    hs.extend(s);
                }
                hs.extend(a.get_lam().unwrap().free_variables(bound));
                hs
            },
            Invoke(i) => {
                i.unwrap().free_variables(bound)
            },
            Match(m) => {
                let mut hs = HashSet::new();
                hs.extend(m.clone().get_expr().unwrap().free_variables(bound));
                let mut new_bound = bound.clone();
                
                // boy howdy am i doing rust correctly ?
                match  m.clone().get_binding().which().unwrap() {
                    crate::core_capnp::expr::match_::binding::Which::BindTo(sym_reader) => {
                        let sym = sym_reader.unwrap();
                        new_bound.insert((sym.get_name().unwrap(), sym.get_disam()));
                    },
                    crate::core_capnp::expr::match_::binding::Which::Omitted(_) => (),
                };
                
                for s in m.get_cases().unwrap()
                        .iter().map(|x| x.get_expr().unwrap().free_variables(&new_bound)) {
                    hs.extend(s);
                }
                hs
            },
            Lam(l) => {
                let mut new_bound = bound.clone();
                for p in l.get_params().unwrap() {
                    let sym = p.get_symbol().unwrap();
                    new_bound.insert((sym.get_name().unwrap(), sym.get_disam()));
                }
                l.get_body().unwrap().free_variables(&new_bound)
            },
            Let(l) => {
                let mut new_bound = bound.clone();

                let binds = l.clone().get_binds().unwrap()
                                .get_binds().unwrap();
                for b in binds.iter() {
                    let sym = b.get_symbol().unwrap();
                    new_bound.insert((sym.get_name().unwrap(), sym.get_disam()));
                }
                let mut hs = HashSet::new();
                for b in binds.iter() {
                    let value = b.get_value().unwrap();
                    hs.extend(value.free_variables(&new_bound));
                }
                hs.extend(l.get_body().unwrap().free_variables(&new_bound));
                hs
            },
            _ => HashSet::new()
        }
    }
}

impl PrettyReader for SymbolReader<'_> {
    fn pretty_doc<'b, D, A>(&self, allocator: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
        where
            D: DocAllocator<'b, A>,
            D::Doc: Clone,
            A: Clone {
        Ok(
            allocator.text(String::from(self.get_name()?))
                .append(":")
                .append(format!("{}", self.get_disam()))
        )
    }
}

impl PrettyReader for PrimitiveReader<'_> {
    fn pretty_doc<'b, D, A>(&self, allocator: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
        where
            D: DocAllocator<'b, A>,
            D::Doc: Clone,
            A: Clone {
        use PrimitiveWhich::*;
        Ok(match self.which()? {
            Unit(_) => allocator.text("()"),
            Bool(b) => allocator.text(if b { "true" } else { "false" }),
            Int(i) => allocator.text(format!("{}", i)),
            Float(f) => allocator.text(format!("{}", f)),
            String(s) => allocator.text(format!("\"{}\"", s?)),
            Char(c) => allocator.text(format!("'{}'", c)),
            Buffer(_) => allocator.text("<buffer>"),
            EmptyList(_) => allocator.text("<empty_list>"),
            EmptyTuple(_) => allocator.text("<empty_tuple>"),
            EmptyRecord(_) => allocator.text("<empty_record>")
        })
    }
}

impl PrettyReader for BindsReader<'_> {
    fn pretty_doc<'b, D, A>(&self, allocator: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
        where
            D: DocAllocator<'b, A>,
            D::Doc: Clone,
            A: Clone {
        let iter = self.get_binds()?.into_iter().map(|x| x.pretty(allocator));
        Ok(
            allocator.intersperse(iter,
        allocator.softline().append("and").append(allocator.softline()))
        )
    }
}

impl PrettyReader for BindReader<'_> {
    fn pretty_doc<'b, D, A>(&self, allocator: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
        where
            D: DocAllocator<'b, A>,
            D::Doc: Clone,
            A: Clone {
        Ok(
            self.get_symbol()?.pretty(allocator)
                .append(allocator.softline())
                .append("=")
                .append(allocator.softline())
                .append(self.get_value()?.pretty(allocator))
        )
    }
}

impl PrettyReader for ApplyReader<'_> {
    fn pretty_doc<'b, D, A>(&self, allocator: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
        where
            D: DocAllocator<'b, A>,
            D::Doc: Clone,
            A: Clone {
        use ApplyWhich::*;
        let val = self.get_value()?;
        Ok(match self.which()? {
            Pos(_) => val.pretty(allocator),
            Key(n) => allocator.text(String::from(n?)).append(":").append(val.pretty(allocator)),
            VarPos(_) => allocator.text("**").append(val.pretty(allocator)),
            VarKey(_) => allocator.text("***").append(val.pretty(allocator)),
        })
    }
}

impl PrettyReader for ParamReader<'_> {
    fn pretty_doc<'b, D, A>(&self, allocator: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
        where
            D: DocAllocator<'b, A>,
            D::Doc: Clone,
            A: Clone {
        use ParamWhich::*;
        let sym = self.get_symbol()?;
        Ok(match self.which()? {
            Pos(_) => sym.pretty(allocator),
            _ => sym.pretty(allocator)
        })
    }
}

impl PrettyReader for ExprReader<'_> {
    fn pretty_doc<'b, D, A>(&self, allocator: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
        where
            D: DocAllocator<'b, A>,
            D::Doc: Clone,
            A: Clone {
        use ExprWhich::*;
        Ok(match self.which()? {
            Id(id) => id?.pretty(allocator),
            Literal(lit) => {
                lit?.pretty_doc(allocator)?
            },
            App(app) => {
                let iter = app.get_args()?.into_iter().map(|x| x.pretty(allocator));
                app.get_lam()?.pretty(allocator)
                    .append(allocator.softline_())
                    .append("@")
                    .append(allocator.softline_())
                    .append(allocator.intersperse(iter, 
                    allocator.softline_().append(",").append(allocator.softline_())
                    ))
            },
            Invoke(inv) => {
                allocator.text("<") 
                    .append(inv?.pretty(allocator))
                    .append(allocator.text(">"))
            },
            Match(_) => {
                allocator.text("match")
            },
            Lam(lam) => {
                let iter = lam.get_params()?.into_iter().map(|x| x.pretty(allocator));
                allocator.text("\\")
                    .append(allocator.intersperse(iter,
                        allocator.text(",").append(allocator.line())))
                    .append(allocator.line())
                    .append("->")
                    .append(allocator.line())
                    .append(lam.get_body()?.pretty(allocator))
            },
            Let(l) => {
                allocator.text("let")
                    .append(allocator.softline())
                    .append(l.get_binds()?.pretty(allocator))
                    .append(allocator.softline())
                    .append("in")
                    .append(allocator.softline())
                    .append("{")
                    .append(l.get_body()?.pretty(allocator))
                    .append("}")
            },
            Error(ce) => {
                allocator.text("[")
                    .append(String::from(ce?.get_summary()?))
                    .append("]")
            },
            InlineBuiltin(r) => {
                let iter = r.get_args()?.into_iter().map(|x| x.pretty(allocator));
                allocator.text("$").append(String::from(r.get_op()?))
                    .append("(").append(
                        allocator.intersperse(iter, 
                                allocator.text(",").append(allocator.line()))
                    ).append(")")
            }
        })
    }
}

// A symbol environment is for turning names into
// unique symbols that don't shadow each other
// TODO: Re-evaluate the need for the disambiguation ID
// under the new framework since we don't do typechecking
pub type DisambID = u32;
pub type Symbol<'e> = (&'e str, DisambID);

#[derive(Clone)]
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