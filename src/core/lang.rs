use std::collections::HashMap;

use super::util::PrettyReader;
use pretty::{DocAllocator, DocBuilder};

pub use crate::core_capnp::expr::{
    Which as ExprWhich,
    Builder as ExprBuilder,
    Reader as ExprReader
};
pub use crate::value_capnp::primitive::{
    Which as PrimitiveWhich,
    Builder as PrimitiveBuilder,
    Reader as PrimitiveReader
};
pub use crate::core_capnp::expr::arg::{
    Which as ArgWhich,
    Builder as ArgBuilder,
    Reader as ArgReader
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

impl PrettyReader for SymbolReader<'_> {
    fn pretty_doc<'b, D, A>(&self, allocator: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
        where
            D: DocAllocator<'b, A>,
            D::Doc: Clone,
            A: Clone {
        Ok(
            allocator.text(String::from(self.get_name()?))
                .append("#")
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
            Buffer(_) => allocator.text("<buffer>")
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

impl PrettyReader for ArgReader<'_> {
    fn pretty_doc<'b, D, A>(&self, allocator: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
        where
            D: DocAllocator<'b, A>,
            D::Doc: Clone,
            A: Clone {
        use ArgWhich::*;
        let val = self.get_value()?;
        Ok(match self.which()? {
            Pos(_) => val.pretty(allocator),
            ByName(n) => allocator.text(String::from(n?)).append("=").append(val.pretty(allocator)),
            VarPos(_) => allocator.text("**").append(val.pretty(allocator)),
            VarKeys(_) => allocator.text("***").append(val.pretty(allocator)),
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
                allocator.text("<")
                    .append(app.get_lam()?.pretty(allocator))
                    .append(">")
                    .append(allocator.line_())
                    .append("@")
                    .append(allocator.line_())
                    .append("(")
                    .append(allocator.intersperse(iter, 
                        allocator.text(",").append(allocator.line())))
                    .append(")").group()
            },
            Call(c) => {
                let iter = c.get_args()?.into_iter().map(|x| x.pretty(allocator));
                allocator.text("<")
                    .append(c.get_lam()?.pretty(allocator))
                    .append(">")
                    .append(allocator.line_())
                    .append("(")
                    .append(allocator.intersperse(iter, 
                        allocator.text(",").append(allocator.line())))
                    .append(")").group()
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
            }
        })
    }
}

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