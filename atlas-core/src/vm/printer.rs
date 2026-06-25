//! Readback / printing of a heap term back into surface-ish syntax.
//!
//! Traversal is by address through the shared-read [`HeapScope::view`], so nothing
//! is consumed. Variables are named by the address of their binder slot.

use crate::vm::heap::{Addr, HeapScope};
use crate::vm::term::Term;
use crate::util::MemoMap;
use std::cell::Cell;
use std::fmt;

pub struct Printer<'a, 'h> {
    heap: &'a HeapScope<'h>,
    var_names: MemoMap<u64, String>,
    name_counter: Cell<usize>,
}

pub struct Pretty<'a, 'h> {
    printer: &'a Printer<'a, 'h>,
    root: Addr,
}

impl fmt::Display for Pretty<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.printer.fmt_addr(f, self.root, true)
    }
}

impl<'a, 'h> Printer<'a, 'h> {
    pub fn new(heap: &'a HeapScope<'h>) -> Self {
        Printer {
            heap,
            var_names: MemoMap::new(),
            name_counter: Cell::new(0),
        }
    }

    pub fn pretty(&'a self, root: Addr) -> Pretty<'a, 'h> {
        Pretty { printer: self, root }
    }

    fn fresh_name(&self) -> String {
        let n = self.name_counter.get();
        self.name_counter.set(n + 1);
        let letter = (b'a' + (n % 26) as u8) as char;
        if n < 26 {
            letter.to_string()
        } else {
            format!("{letter}{}", n / 26)
        }
    }

    fn var_name(&self, addr: Addr) -> &str {
        self.var_names
            .get_or_insert_with(addr.to_u64(), || self.fresh_name())
            .as_str()
    }

    fn fmt_addr(&self, f: &mut fmt::Formatter<'_>, addr: Addr, tail: bool) -> fmt::Result {
        match self.heap.view(addr) {
            Term::Var => write!(f, "{}", self.var_name(addr)),
            Term::Lam { body } => {
                if !tail {
                    write!(f, "(")?;
                }
                write!(f, "\\{} -> ", self.var_name(body.binder_addr()))?;
                self.fmt_addr(f, body.body_addr(), true)?;
                if !tail {
                    write!(f, ")")?;
                }
                Ok(())
            }
            Term::Use { body } => {
                if !tail {
                    write!(f, "(")?;
                }
                write!(f, "\\_ -> ")?;
                self.fmt_addr(f, body.addr(), true)?;
                if !tail {
                    write!(f, ")")?;
                }
                Ok(())
            }
            Term::App { func, arg } => {
                write!(f, "(")?;
                self.fmt_addr(f, func.addr(), false)?;
                write!(f, " ")?;
                self.fmt_addr(f, arg.addr(), false)?;
                write!(f, ")")
            }
            Term::Bop { op, lhs, rhs } => {
                write!(f, "(")?;
                self.fmt_addr(f, lhs.addr(), false)?;
                write!(f, " {} ", op.symbol())?;
                self.fmt_addr(f, rhs.addr(), false)?;
                write!(f, ")")
            }
            Term::Ctr {
                name,
                arity,
                values,
            } => {
                let nm = self.heap.name(&name);
                if nm == "Nil" && arity == 0 {
                    return write!(f, "[]");
                }
                if arity == 0 {
                    return write!(f, "#{nm}");
                }
                write!(f, "#{nm}{{")?;
                for i in 0..arity as usize {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    self.fmt_addr(f, self.heap.pack_field(&values, i).addr(), false)?;
                }
                write!(f, "}}")
            }
            Term::U64(n) => write!(f, "{n}"),
            Term::I64(n) => write!(f, "{n}"),
            Term::F32(x) => write!(f, "{x}"),
            Term::F64(x) => write!(f, "{x}"),
            Term::Char(c) => write!(f, "{c:?}"),
            Term::Bool(b) => write!(f, "{b}"),
            Term::Sup { ptr, .. } => {
                let (a, b) = self.heap.sup_args(&ptr);
                write!(f, "&{{")?;
                self.fmt_addr(f, a.addr(), false)?;
                write!(f, ", ")?;
                self.fmt_addr(f, b.addr(), false)?;
                write!(f, "}}")
            }
            Term::Wld => write!(f, "*"),
            Term::Pri(id) => write!(f, "%{}", id.get()),
            _ => write!(f, "<?>"),
        }
    }
}
