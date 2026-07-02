//! Pretty-printing of the desugared core [`Expr`] in surface-ish syntax.
//!
//! The analogue of the VM's heap readback ([`crate::vm::printer`]), but for the
//! lexically-scoped core IR: `Expr` uses de Bruijn binders (each `Lam`/`Use`/`Dup`
//! introduces one level), so binders are named through an env stack and resolved
//! by index. Unlike the heap printer — which must hoist dups out of a shared graph
//! — an [`Expr::Dup`] is printed inline at its lexical position as a
//! `&{a, b} = val; body` binding.
//!
//! Labels follow the source: a `Named` dup/sup prints its label (`&L{..}`), an
//! `Auto` (generated) one prints none (`&{..}`).

use std::fmt;

use crate::core::expr::{DeBruijn, Expr, Pat, TypeDefKind, Value};

/// Render an `f64` so it always carries a decimal point (e.g. `3.0`, not `3`),
/// keeping floats visually distinct from ints. `inf`/`NaN` print as-is.
pub fn fmt_float(f: &mut fmt::Formatter<'_>, x: f64) -> fmt::Result {
    let s = format!("{x}");
    if s.contains(['.', 'e', 'E']) || !x.is_finite() {
        write!(f, "{s}")
    } else {
        write!(f, "{s}.0")
    }
}

/// Render a builtin [`Value`] in surface syntax.
pub fn fmt_value(f: &mut fmt::Formatter<'_>, v: &Value) -> fmt::Result {
    match v {
        Value::Int(n) => write!(f, "{n}"),
        Value::Float(x) => fmt_float(f, x.into_inner()),
        Value::Char(c) => write!(f, "{c:?}"),
        Value::Bool(b) => write!(f, "{b}"),
        Value::Str(s) => write!(f, "{s:?}"),
        Value::Bytes(b) => write!(f, "{b:?}"),
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Namer {
            counter: 0,
            env: Vec::new(),
        }
        .go(f, self, true)
    }
}

/// One de Bruijn binder level, holding the name(s) assigned to it.
enum Binder {
    /// a `Lam` binder (named by `Var`).
    Lam(String),
    /// an erasing (`Use`) binder: occupies a level but is never referenced.
    Erased,
    /// a `Dup` binder: the (single) name used for every projection (`Ref`).
    Dup(String),
}

struct Namer {
    counter: usize,
    env: Vec<Binder>,
}

impl Namer {
    /// A fresh variable name: `a..z`, then `a1`, `b1`, … (matches the heap
    /// printer's scheme in `vm/printer.rs`).
    fn fresh(&mut self) -> String {
        let n = self.counter;
        self.counter += 1;
        let letter = (b'a' + (n % 26) as u8) as char;
        if n < 26 {
            letter.to_string()
        } else {
            format!("{letter}{}", n / 26)
        }
    }

    /// The binder selected by a de Bruijn index (0 = innermost).
    fn at(&self, db: DeBruijn) -> Option<&Binder> {
        let len = self.env.len();
        len.checked_sub(1 + db.0 as usize).map(|i| &self.env[i])
    }

    fn go(&mut self, f: &mut fmt::Formatter<'_>, e: &Expr, tail: bool) -> fmt::Result {
        match e {
            Expr::Var(db) => match self.at(*db) {
                Some(Binder::Lam(name)) => write!(f, "{name}"),
                _ => write!(f, "<var:{}>", db.0),
            },
            Expr::Ref(db) => match self.at(*db) {
                Some(Binder::Dup(name)) => write!(f, "{name}"),
                _ => write!(f, "<ref:{}>", db.0),
            },
            Expr::Era => write!(f, "&{{}}"),
            Expr::Wld => write!(f, "*"),
            Expr::Value(v) => fmt_value(f, v),
            Expr::Free(name) => write!(f, "{name}"),
            Expr::Pri(name) => write!(f, "%{name}"),
            Expr::Sup { left, right } => {
                write!(f, "&{{")?;
                self.go(f, left, false)?;
                write!(f, ", ")?;
                self.go(f, right, false)?;
                write!(f, "}}")
            }
            Expr::Dup { val, body } => {
                if !tail {
                    write!(f, "(")?;
                }
                let name = self.fresh();
                // The value is in scope outside the dup's own binder. It sits
                // between `=` and `;`, so a `Lam`/`Use` value needs no parens
                // (render as tail); a nested `Dup` value does, to keep its own `;`
                // unambiguous.
                let val_tail = !matches!(val.as_ref(), Expr::Dup { .. });
                write!(f, "&{{{name}}} = ")?;
                self.go(f, val, val_tail)?;
                write!(f, ";{}", if tail { '\n' } else { ' ' })?;
                self.env.push(Binder::Dup(name));
                let r = self.go(f, body, tail);
                self.env.pop();
                r?;
                if !tail {
                    write!(f, ")")?;
                }
                Ok(())
            }
            Expr::Lam { body } => {
                if !tail {
                    write!(f, "(")?;
                }
                let name = self.fresh();
                write!(f, "\\{name} -> ")?;
                self.env.push(Binder::Lam(name));
                let r = self.go(f, body, true);
                self.env.pop();
                r?;
                if !tail {
                    write!(f, ")")?;
                }
                Ok(())
            }
            Expr::Use { body } => {
                if !tail {
                    write!(f, "(")?;
                }
                write!(f, "\\_ -> ")?;
                self.env.push(Binder::Erased);
                let r = self.go(f, body, true);
                self.env.pop();
                r?;
                if !tail {
                    write!(f, ")")?;
                }
                Ok(())
            }
            Expr::App { func, arg } => {
                write!(f, "(")?;
                self.go(f, func, false)?;
                write!(f, " ")?;
                self.go(f, arg, false)?;
                write!(f, ")")
            }
            Expr::Bop { op, left, right } => {
                write!(f, "(")?;
                self.go(f, left, false)?;
                write!(f, " {} ", op.symbol())?;
                self.go(f, right, false)?;
                write!(f, ")")
            }
            Expr::Uop { op, val } => {
                write!(f, "({}", op.symbol())?;
                self.go(f, val, false)?;
                write!(f, ")")
            }
            Expr::Ctr { ty, variant } => {
                self.go(f, ty, false)?;
                match variant {
                    Some(name) => write!(f, "::{name}"),
                    None => write!(f, "::New"),
                }
            }
            Expr::TypeDef { kind } => match kind {
                TypeDefKind::Product(fields) => {
                    write!(f, "type(")?;
                    for (i, t) in fields.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        self.go(f, t, false)?;
                    }
                    write!(f, ")")
                }
                TypeDefKind::Sum(variants) => {
                    write!(f, "type{{")?;
                    for (i, (name, args)) in variants.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        write!(f, "{name}")?;
                        if !args.is_empty() {
                            write!(f, "(")?;
                            for (j, a) in args.iter().enumerate() {
                                if j > 0 {
                                    write!(f, ", ")?;
                                }
                                self.go(f, a, false)?;
                            }
                            write!(f, ")")?;
                        }
                    }
                    write!(f, "}}")
                }
            },
            Expr::Mat { cases, default } => {
                write!(f, "?{{")?;
                let mut first = true;
                for (pat, body) in cases {
                    if !first {
                        write!(f, "; ")?;
                    }
                    first = false;
                    match pat {
                        Pat::Ctr(n) if n == "Nil" => write!(f, "[]")?,
                        Pat::Ctr(n) => write!(f, "{n}")?,
                        Pat::Val(v) => fmt_value(f, v)?,
                    }
                    write!(f, " -> ")?;
                    self.go(f, body, true)?;
                }
                if let Some(d) = default {
                    if !first {
                        write!(f, "; ")?;
                    }
                    // The default is a lambda applied to the scrutinee: an erasing
                    // `Use` prints as `_ -> body`, a `Lam` binds a fresh name, and
                    // anything else is the bare use-form `?{ term }`.
                    match d.as_ref() {
                        Expr::Use { body } => {
                            write!(f, "_ -> ")?;
                            self.env.push(Binder::Erased);
                            let r = self.go(f, body, true);
                            self.env.pop();
                            r?;
                        }
                        Expr::Lam { body } => {
                            let name = self.fresh();
                            write!(f, "{name} -> ")?;
                            self.env.push(Binder::Lam(name));
                            let r = self.go(f, body, true);
                            self.env.pop();
                            r?;
                        }
                        other => self.go(f, other, true)?,
                    }
                }
                write!(f, "}}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::core::ast::desugar;
    use crate::core::parse::parse;

    fn pp(src: &str) -> String {
        desugar(&parse(src).unwrap()).unwrap().to_string()
    }

    #[test]
    fn cloned_let_prints_dup_binding() {
        // auto-dup: one shared value, every projection prints the same name.
        assert_eq!(
            pp(r"&x = \y -> 2 * y; (x 1) + (x 2)"),
            "&{a} = \\b -> (2 * b);\n((a 1) + (a 2))"
        );
    }

    #[test]
    fn auto_dup_is_a_single_inline_dup() {
        // A cloned binder is a single dup; each use is a projection of it.
        assert_eq!(
            pp(r"\&x -> x + x + x"),
            "\\a -> &{b} = a;\n((b + b) + b)"
        );
    }

    #[test]
    fn explicit_dup_prints_single_name() {
        assert_eq!(pp(r"&{p q} = 5; p + q"), "&{a} = 5;\n(a + a)");
    }

    #[test]
    fn basic_shapes() {
        assert_eq!(pp(r"\x -> x + 1"), "\\a -> (a + 1)");
        assert_eq!(pp(r"?{1 -> 100; 2 -> 200}"), "?{1 -> 100; 2 -> 200}");
    }

    #[test]
    fn string_and_char_are_values() {
        // No desugaring into character lists / `Chr` constructors.
        assert_eq!(pp(r#""hi""#), r#""hi""#);
        assert_eq!(pp(r"'a'"), "'a'");
    }
}
