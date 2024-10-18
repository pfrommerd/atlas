use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

pub use crate::Constant;
pub use super::Type;

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub enum Tree {
    Erase,
    Constant(Constant),
    // For wiring
    Var(String),
    // Reference to a net
    Ref(String),
    // Builtin operator
    Operator(String, Vec<Constant>, Vec<Tree>),
    Con(Option<Type>, Vec<Tree>),
    Dup(Vec<Tree>),
    // switch equality cases
    // Switch(Vec<(Constant, Tree)>),
}

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub struct Redex {
    pub lhs: Tree,
    pub rhs: Tree
}

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub struct Net {
  pub root: Tree,
  pub redexs: Vec<Redex>,
}


#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub struct Book {
    pub defs: BTreeMap<String, Net>,
}

impl Display for Type {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "{0}{1}", self.0, match &self.1 {
            Some(t) => format!("#{}", t),
            None => "".to_string()
        })
    }
}

impl Display for Tree {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        match self {
            Tree::Constant(c) => write!(f, "{c}"),
            Tree::Var(v) => write!(f, "{v}"),
            Tree::Ref(r) => write!(f, "@{r}"),
            Tree::Erase => write!(f, "*"),
            Tree::Operator(op, meta_args, args) => {
                write!(f, "${op}")?;
                if !args.is_empty() {
                    write!(f, "[")?;
                    let mut first = true;
                    for arg in args {
                        if first { write!(f, "{}", arg)?; } 
                        else { write!(f, ", {}", arg)?; }
                        first = false;
                    }
                    write!(f, "]")?;
                }
                Ok(())
            },
            Tree::Con(ty, args) => {
                if let Some(ty) = ty { write!(f, "{ty}")?; }
                write!(f, "(")?;
                if args.is_empty() { write!(f, ",")?; }
                let mut first = true;
                for arg in args {
                    if first { write!(f, "{}", arg)?; } 
                    else { write!(f, ", {}", arg)?; }
                    first = false;
                }
                write!(f, ")")
            },
            Tree::Dup(args) => {
                write!(f, "{{")?;
                let mut first = true;
                for arg in args {
                    if first { write!(f, "{}", arg)?; } 
                    else { write!(f, " {}", arg)?; }
                    first = false;
                }
                write!(f, "}}")
            }
        }
    }
}

impl Display for Redex {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "{lhs} ~ {rhs}", lhs = self.lhs, rhs = self.rhs)
    }
}

impl Display for Net {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        write!(f, "{root}", root = self.root)?;
        for redex in &self.redexs {
            write!(f, " &\n\t {redex}", redex = redex)?;
        }
        Ok(())
    }
}

impl Display for Book {
    fn fmt(&self, f: &mut Formatter) -> std::fmt::Result {
        for (name, net) in &self.defs {
            write!(f, "@{name} = {net}\n")?;
        }
        Ok(())
    }
}
