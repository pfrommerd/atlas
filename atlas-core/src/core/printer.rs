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

use crate::core::expr::{DeBruijn, Expr, Label, Pat, Value};

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
    /// a `Dup` binder: the two projection names (`Dp0`, `Dp1`).
    Dup(String, String),
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

    /// The label text printed between `&` and `{` (empty for `Auto`).
    fn label(label: &Label) -> &str {
        match label {
            Label::Named(s) => s,
            Label::Auto => "",
        }
    }

    fn go(&mut self, f: &mut fmt::Formatter<'_>, e: &Expr, tail: bool) -> fmt::Result {
        match e {
            Expr::Var(db) => match self.at(*db) {
                Some(Binder::Lam(name)) => write!(f, "{name}"),
                _ => write!(f, "<var:{}>", db.0),
            },
            Expr::Dp0(db) => match self.at(*db) {
                Some(Binder::Dup(a, _)) => write!(f, "{a}"),
                _ => write!(f, "<dp0:{}>", db.0),
            },
            Expr::Dp1(db) => match self.at(*db) {
                Some(Binder::Dup(_, b)) => write!(f, "{b}"),
                _ => write!(f, "<dp1:{}>", db.0),
            },
            Expr::Era => write!(f, "&{{}}"),
            Expr::Wld => write!(f, "*"),
            Expr::Value(v) => fmt_value(f, v),
            Expr::Ref(name) => write!(f, "@{name}"),
            Expr::Free(name) => write!(f, "{name}"),
            Expr::Pri(name) => write!(f, "%{name}"),
            Expr::Sup { label, left, right } => {
                write!(f, "&{}{{", Self::label(label))?;
                self.go(f, left, false)?;
                write!(f, ", ")?;
                self.go(f, right, false)?;
                write!(f, "}}")
            }
            Expr::Dup { label, val, body } => {
                if !tail {
                    write!(f, "(")?;
                }
                let (a, b) = (self.fresh(), self.fresh());
                // The value is in scope outside the dup's own binder. It sits
                // between `=` and `;`, so a `Lam`/`Use` value needs no parens
                // (render as tail); a nested `Dup` value does, to keep its own `;`
                // unambiguous.
                let val_tail = !matches!(val.as_ref(), Expr::Dup { .. });
                write!(f, "&{}{{{a}, {b}}} = ", Self::label(label))?;
                self.go(f, val, val_tail)?;
                write!(f, ";{}", if tail { '\n' } else { ' ' })?;
                self.env.push(Binder::Dup(a, b));
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
            Expr::Ctr { name, args } => {
                if args.is_empty() {
                    return if name == "Nil" {
                        write!(f, "[]")
                    } else {
                        write!(f, "{name}")
                    };
                }
                write!(f, "{name}{{")?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    self.go(f, arg, false)?;
                }
                write!(f, "}}")
            }
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
                    write!(f, "_ -> ")?;
                    self.go(f, d, true)?;
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
        // auto-dup: no label, value shared across both projections.
        assert_eq!(
            pp(r"&x = \y -> 2 * y; (x 1) + (x 2)"),
            "&{a, b} = \\c -> (2 * c);\n((a 1) + (b 2))"
        );
    }

    #[test]
    fn auto_dup_chain_is_inline_and_ordered() {
        // Dups nest inline under the lambda; the second's value is the first's Dp1.
        assert_eq!(
            pp(r"\&x -> x + x + x"),
            "\\a -> &{b, c} = a;\n&{d, e} = c;\n((b + d) + e)"
        );
    }

    #[test]
    fn named_dup_keeps_label() {
        assert_eq!(pp(r"&L{p q} = 5; p + q"), "&L{a, b} = 5;\n(a + b)");
    }

    #[test]
    fn basic_shapes() {
        assert_eq!(pp(r"\x -> x + 1"), "\\a -> (a + 1)");
        assert_eq!(pp(r"[1, 2]"), "Con{1, Con{2, []}}");
        assert_eq!(pp(r"?{1 -> 100; 2 -> 200}"), "?{1 -> 100; 2 -> 200}");
    }

    #[test]
    fn string_and_char_are_values() {
        // No desugaring into character lists / `Chr` constructors.
        assert_eq!(pp(r#""hi""#), r#""hi""#);
        assert_eq!(pp(r"'a'"), "'a'");
    }
}
