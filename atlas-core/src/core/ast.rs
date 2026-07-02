use std::collections::HashMap;

use ordered_float::OrderedFloat;

use crate::core::expr::{DeBruijn, Expr, Pat, TypeDefKind, Value};
use crate::vm::term::{BinaryOp, UnaryOp};

#[rustfmt::skip]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfixOp {
    Add, Sub, Mul,
    Div, IDiv, Mod,
    And, Or, Xor,
    Shl, Shr, Eq, Neq,
    Lt, Lte, Gt, Gte, Cons,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal<'src> {
    Integer(u64),
    Float(OrderedFloat<f64>),
    Bool(bool),
    Char(char),
    String(&'src str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern<'src> {
    Ctr(&'src str),
    Lit(Literal<'src>),
    // []: and <>:
    Nil,
    Cons,
    // _: wildcard
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding<'src> {
    Hole, // _
    // x (or &x for auto-dup)
    Var {
        name: &'src str,
        auto_dup: bool,
    },
    // &{a, b, c} for an explicit dup (all names share one duplication)
    Dup {
        names: Vec<&'src str>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[rustfmt::skip]
pub enum Node<'src> {
    // literal
    Lit { val: Literal<'src> },
    // List literal [node, node, ...]
    // gets desugared to Cons{node, Cons{node, Cons{node, Nil}}}
    List { elems: Vec<Node<'src>> },
    /// variable: Foo or Foo#0 or Foo#1 for dup variables
    Var { name: &'src str },
    // builtin primitive: %Foo
    Primitive { name: &'src str },
    /// superposition: `"&" "{" (Node ",")* Node "}"`
    Sup { nodes: Vec<Node<'src>> },
    /// duplication term:
    // ! x = y; a + b
    Let { bindings: Vec<(Binding<'src>, Node<'src>)>, body: Box<Node<'src>>, },
    // \ &x -> x + x
    Lambda { binders: Vec<Binding<'src>>, body: Box<Node<'src>>, },
    /// erasure: `"&{}"` or equivalently, `\{}`
    Erase,
    /// product/tuple type declaration: `"type" "(" typeExpr,* ")"`.
    ProductType { fields: Vec<Node<'src>> },
    /// sum/enum type declaration: `"type" "{" Variant,* "}"`, where a variant is
    /// `Name` (nullary) or `Name( argTypes )`.
    SumType { variants: Vec<(&'src str, Vec<Node<'src>>)> },
    /// constructor selector: `Node "::" Name`. `variant` is `None` for the
    /// product constructor (`::New`) and `Some(name)` for a sum variant.
    Ctr { ty: Box<Node<'src>>, variant: Option<&'src str> },
    /// pattern match: `"?""{" (Pattern Binding* "->" Term ";")* Term "}"`
    /// note that ?{Term} or ?{_ -> Term} applies the unboxed value to Term
    /// i.e. ?{\x -> x} #Some{1} ==> 1
    /// field binders after a pattern are sugar for a lambda over the
    /// constructor's fields: `?{Con x y -> body}` == `?{Con -> \x y -> body}`.
    /// a `_ -> Term` case is routed to `default`.
    Match { cases: Vec<(Pattern<'src>, Node<'src>)>, default: Option<Box<Node<'src>>> },
    /// f a
    App { func: Box<Node<'src>>, args: Vec<Node<'src>>, },
    /// infix operation: Node Oper Term
    Infix { left: Box<Node<'src>>, op: InfixOp, right: Box<Node<'src>>, },
    /// prefix unary operation: Oper Node
    Unary { op: UnaryOp, expr: Box<Node<'src>>, },
    /// wildcard: `*`
    Wild,
}

// ========================================================================
// Lowering: surface AST (`Node`) -> desugared core IR (`Expr`)
// ========================================================================

/// Lower a surface AST node into a desugared [`Expr`].
pub fn desugar<'n>(node: &'n Node<'n>) -> Result<Expr, String> {
    let mut d = Desugar {
        depth: 0,
        env: HashMap::new(),
        allow_free: false,
    };
    d.go(node)
}

/// Like [`desugar`], but unbound names lower to [`Expr::Free`] (resolved later,
/// e.g. against a REPL's local bindings) instead of erroring on the spot.
pub fn desugar_open<'n>(node: &'n Node<'n>) -> Result<Expr, String> {
    let mut d = Desugar {
        depth: 0,
        env: HashMap::new(),
        allow_free: true,
    };
    d.go(node)
}

/// What a source name resolves to during desugaring.
#[derive(Clone, Copy)]
enum BindingDesugar<'n> {
    /// a `Lam` binder bound at this absolute depth
    Lam(usize),
    /// a `Dup` binder bound at this absolute depth. Every use of the name is a
    /// fresh projection (`Ref`) of that single dup.
    Dup(usize),
    /// an affine let binding, inlined (re-desugared) at every use. It should only be used once.
    Let(&'n Node<'n>),
}

struct Desugar<'n> {
    depth: usize,
    env: HashMap<&'n str, BindingDesugar<'n>>,
    /// When set, an unbound name lowers to [`Expr::Free`] rather than erroring.
    allow_free: bool,
}

impl<'n> Desugar<'n> {
    fn go(&mut self, node: &'n Node<'n>) -> Result<Expr, String> {
        match node {
            Node::Lit { val } => Ok(self.lit(val)),
            Node::List { elems } => self.list(elems),
            Node::Var { name } => self.use_var(name),
            Node::Primitive { name } => Ok(Expr::Pri(name.to_string())),
            Node::Wild => Ok(Expr::Wld),
            Node::Erase => Ok(Expr::Era),
            Node::Sup { nodes } => {
                if nodes.len() != 2 {
                    return Err("superposition must have exactly two elements".into());
                }
                let left = self.go(&nodes[0])?;
                let right = self.go(&nodes[1])?;
                Ok(Expr::Sup {
                    left: Box::new(left),
                    right: Box::new(right),
                })
            }
            Node::ProductType { fields } => {
                let mut fs = Vec::with_capacity(fields.len());
                for n in fields {
                    fs.push(self.go(n)?);
                }
                Ok(Expr::TypeDef {
                    kind: TypeDefKind::Product(fs),
                })
            }
            Node::SumType { variants } => {
                if variants.is_empty() {
                    return Err("a variant type `type { .. }` must have at least one variant".into());
                }
                let mut vs = Vec::with_capacity(variants.len());
                for (name, args) in variants {
                    if *name == "New" {
                        return Err("`New` is reserved as the product constructor and cannot be used as a variant name".into());
                    }
                    let mut a = Vec::with_capacity(args.len());
                    for arg in args {
                        a.push(self.go(arg)?);
                    }
                    vs.push((name.to_string(), a));
                }
                Ok(Expr::TypeDef {
                    kind: TypeDefKind::Sum(vs),
                })
            }
            Node::Ctr { ty, variant } => Ok(Expr::Ctr {
                ty: Box::new(self.go(ty)?),
                variant: variant.map(str::to_string),
            }),
            Node::App { func, args } => {
                let mut f = self.go(func)?;
                for arg in args {
                    let x = self.go(arg)?;
                    f = Expr::App {
                        func: Box::new(f),
                        arg: Box::new(x),
                    };
                }
                Ok(f)
            }
            Node::Infix { left, op, right } => {
                let l = self.go(left)?;
                let r = self.go(right)?;
                if let InfixOp::Cons = op {
                    return Ok(cons(l, r));
                }
                Ok(Expr::Bop {
                    op: op.try_into().expect("Unexpected Cons in Bop!"),
                    left: Box::new(l),
                    right: Box::new(r),
                })
            }
            Node::Unary { op, expr } => Ok(Expr::Uop {
                op: *op,
                val: Box::new(self.go(expr)?),
            }),
            Node::Lambda { binders, body } => self.lam(binders, body),
            Node::Let { bindings, body } => self.lets(bindings, 0, body),
            Node::Match { cases, default } => {
                let mut compiled = Vec::with_capacity(cases.len());
                let mut def = match default {
                    Some(d) => Some(self.go(d)?),
                    None => None,
                };
                for (pat, body) in cases {
                    if matches!(pat, Pattern::Default) {
                        if def.is_some() {
                            return Err("match has more than one default branch".into());
                        }
                        def = Some(self.go(body)?);
                    } else {
                        compiled.push((pat_key(pat)?, self.go(body)?));
                    }
                }
                Ok(Expr::Mat {
                    cases: compiled,
                    default: def.map(Box::new),
                })
            }
        }
    }

    fn lit(&mut self, lit: &Literal) -> Expr {
        Expr::Value(lit_value(lit))
    }

    fn list(&mut self, elems: &'n [Node<'n>]) -> Result<Expr, String> {
        let mut acc = nil();
        for e in elems.iter().rev() {
            let head = self.go(e)?;
            acc = cons(head, acc);
        }
        Ok(acc)
    }

    /// Resolve a variable use (de Bruijn), driving auto-dup and let inlining.
    fn use_var(&mut self, name: &'n str) -> Result<Expr, String> {
        enum What<'n> {
            Lam(usize),
            Dup(usize),
            Inline(&'n Node<'n>),
        }
        let what = match self.env.get(name) {
            None => {
                // Unbound names are free (resolved later, e.g. by the prelude or a
                // REPL local) when allowed, else an error.
                if self.allow_free {
                    return Ok(Expr::Free(name.to_string()));
                }
                return Err(format!("unbound variable `{name}`"));
            }
            Some(BindingDesugar::Lam(d)) => What::Lam(*d),
            Some(BindingDesugar::Dup(d)) => What::Dup(*d),
            Some(BindingDesugar::Let(n)) => What::Inline(*n),
        };
        let depth = self.depth;
        let idx = |d: usize| DeBruijn((depth - 1 - d) as u64);
        Ok(match what {
            // Each use of a dup binder is a distinct projection wire.
            What::Lam(d) => Expr::Var(idx(d)),
            What::Dup(d) => Expr::Ref(idx(d)),
            What::Inline(n) => self.go(n)?,
        })
    }

    #[rustfmt::skip]
    fn lam(&mut self, binders: &'n [Binding<'n>], body: &'n Node<'n>) -> Result<Expr, String> {
        let (binder, rest) = match binders.split_first() {
            Some(x) => x,
            None => return self.go(body),
        };
        match binder {
            Binding::Hole => {
                // erasing binder: a Lam whose variable is never referenced
                self.depth += 1;
                let inner = self.lam(rest, body);
                self.depth -= 1;
                Ok(Expr::Use {
                    body: Box::new(inner?),
                })
            }
            Binding::Var { name, auto_dup: false } => {
                let n = count_in_rest(rest, body, name);
                if n > 1 {
                    return Err(format!(
                        "affine variable `{name}` used {n} times; use `&{name}`"
                    ));
                }
                self.depth += 1;
                if n == 0 {
                    // unused binder: an erasing lambda
                    let inner = self.lam(rest, body);
                    self.depth -= 1;
                    return Ok(Expr::Use {
                        body: Box::new(inner?),
                    });
                }
                let lam_depth = self.depth - 1;
                let prev = self.env.insert(name, BindingDesugar::Lam(lam_depth));
                let inner = self.lam(rest, body);
                restore(&mut self.env, name, prev);
                self.depth -= 1;
                Ok(Expr::Lam {
                    body: Box::new(inner?),
                })
            }
            Binding::Var { name, auto_dup: true } => {
                self.lam_dup(std::slice::from_ref(name), rest, body)
            }
            Binding::Dup { names } => self.lam_dup(names, rest, body),
        }
    }

    /// Lower a cloned lambda binder (`\&x` or an explicit `\&{a, b}`): the
    /// lambda's argument is duplicated by a single dup, and every use of any
    /// bound name is a distinct projection of it. Degenerate arities collapse:
    /// zero uses is an erasing lambda, a single use is a plain (linear) binder.
    #[rustfmt::skip]
    fn lam_dup(
        &mut self,
        names: &[&'n str],
        rest: &'n [Binding<'n>],
        body: &'n Node<'n>,
    ) -> Result<Expr, String> {
        let count: usize = names.iter().map(|n| count_in_rest(rest, body, n)).sum();
        if count == 0 {
            // unused binder: an erasing lambda
            self.depth += 1;
            let inner = self.lam(rest, body);
            self.depth -= 1;
            return Ok(Expr::Use { body: Box::new(inner?) });
        }
        if count == 1 {
            // a single use needs no dup: a plain lambda binder
            let lam_depth = self.depth;
            self.depth += 1;
            let prevs = self.bind_all(names, BindingDesugar::Lam(lam_depth));
            let inner = self.lam(rest, body);
            self.unbind_all(prevs);
            self.depth -= 1;
            return Ok(Expr::Lam { body: Box::new(inner?) });
        }
        // count >= 2: the lambda binder plus a single dup over its argument.
        self.depth += 1; // the lambda's own binder
        let dup_depth = self.depth;
        self.depth += 1; // the dup binder
        let prevs = self.bind_all(names, BindingDesugar::Dup(dup_depth));
        let inner = self.lam(rest, body);
        self.unbind_all(prevs);
        self.depth -= 2;
        let dup = Expr::Dup {
            val: Box::new(Expr::Var(DeBruijn(0))), // the lambda's argument
            body: Box::new(inner?),
        };
        Ok(Expr::Lam { body: Box::new(dup) })
    }

    /// Bind every name to (a copy of) `to`, returning the shadowed entries for
    /// [`Self::unbind_all`]. `to` is `Copy` so all names share one duplication.
    fn bind_all(
        &mut self,
        names: &[&'n str],
        to: BindingDesugar<'n>,
    ) -> Vec<(&'n str, Option<BindingDesugar<'n>>)> {
        names
            .iter()
            .map(|n| (*n, self.env.insert(n, to)))
            .collect()
    }

    fn unbind_all(&mut self, prevs: Vec<(&'n str, Option<BindingDesugar<'n>>)>) {
        for (n, prev) in prevs.into_iter().rev() {
            restore(&mut self.env, n, prev);
        }
    }

    fn lets(
        &mut self,
        bindings: &'n [(Binding<'n>, Node<'n>)],
        idx: usize,
        body: &'n Node<'n>,
    ) -> Result<Expr, String> {
        if idx >= bindings.len() {
            return self.go(body);
        }
        let (binder, val) = &bindings[idx];
        match binder {
            Binding::Hole => {
                // erased let: drop the value
                self.lets(bindings, idx + 1, body)
            }
            Binding::Var {
                name,
                auto_dup: false,
            } => {
                let n = count_seq(&bindings[idx + 1..], body, name);
                if n > 1 {
                    return Err(format!(
                        "affine variable `{name}` used {n} times; use `&{name}`"
                    ));
                }
                let prev = self.env.insert(name, BindingDesugar::Let(val));
                let r = self.lets(bindings, idx + 1, body);
                restore(&mut self.env, name, prev);
                r
            }
            Binding::Var {
                name,
                auto_dup: true,
            } => self.lets_dup(std::slice::from_ref(name), val, bindings, idx, body),
            Binding::Dup { names } => self.lets_dup(names, val, bindings, idx, body),
        }
    }

    /// Lower a cloned (`&x`) or explicit (`&{a, b}`) let binding: one shared
    /// value, duplicated by a single dup, with every use a distinct projection.
    /// Degenerate arities collapse: zero uses drops the value, a single use
    /// inlines it at its one site (no dup).
    fn lets_dup(
        &mut self,
        names: &[&'n str],
        val: &'n Node<'n>,
        bindings: &'n [(Binding<'n>, Node<'n>)],
        idx: usize,
        body: &'n Node<'n>,
    ) -> Result<Expr, String> {
        let rest = &bindings[idx + 1..];
        let count: usize = names.iter().map(|n| count_seq(rest, body, n)).sum();
        if count == 0 {
            // erased: drop the value
            return self.lets(bindings, idx + 1, body);
        }
        if count == 1 {
            // a single use needs no dup: inline the value at its one site
            let prevs = self.bind_all(names, BindingDesugar::Let(val));
            let r = self.lets(bindings, idx + 1, body);
            self.unbind_all(prevs);
            return r;
        }
        // count >= 2: the value is duplicated, not re-desugared, so lower it once
        // here (in the scope outside the dup binder).
        let val_expr = self.go(val)?;
        let dup_depth = self.depth;
        self.depth += 1;
        let prevs = self.bind_all(names, BindingDesugar::Dup(dup_depth));
        let inner = self.lets(bindings, idx + 1, body);
        self.unbind_all(prevs);
        self.depth -= 1;
        Ok(Expr::Dup {
            val: Box::new(val_expr),
            body: Box::new(inner?),
        })
    }
}

fn restore<'n>(
    env: &mut HashMap<&'n str, BindingDesugar<'n>>,
    name: &'n str,
    prev: Option<BindingDesugar<'n>>,
) {
    match prev {
        Some(b) => {
            env.insert(name, b);
        }
        None => {
            env.remove(name);
        }
    }
}

impl TryInto<BinaryOp> for &InfixOp {
    type Error = ();

    fn try_into(self) -> Result<BinaryOp, Self::Error> {
        Ok(match self {
            InfixOp::Add => BinaryOp::Add,
            InfixOp::Sub => BinaryOp::Sub,
            InfixOp::Mul => BinaryOp::Mul,
            InfixOp::Div => BinaryOp::Div,
            InfixOp::IDiv => BinaryOp::IDiv,
            InfixOp::Mod => BinaryOp::Mod,
            InfixOp::And => BinaryOp::And,
            InfixOp::Or => BinaryOp::Or,
            InfixOp::Xor => BinaryOp::Xor,
            InfixOp::Shl => BinaryOp::Shl,
            InfixOp::Shr => BinaryOp::Shr,
            InfixOp::Eq => BinaryOp::Eq,
            InfixOp::Neq => BinaryOp::Neq,
            InfixOp::Lt => BinaryOp::Lt,
            InfixOp::Lte => BinaryOp::Lte,
            InfixOp::Gt => BinaryOp::Gt,
            InfixOp::Gte => BinaryOp::Gte,
            InfixOp::Cons => return Err(()),
        })
    }
}

/// Lower a source `Literal` to a core `Value`.
fn lit_value(lit: &Literal) -> Value {
    match lit {
        Literal::Integer(i) => Value::Int(*i as i64),
        Literal::Float(x) => Value::Float(*x),
        Literal::Bool(b) => Value::Bool(*b),
        Literal::Char(c) => Value::Char(*c),
        Literal::String(s) => Value::Str((*s).to_string()),
    }
}

/// The list element type used by list sugar. Lists are monomorphic to `Int` for
/// now; `List` and `Int` are free names resolved later by the prelude.
fn list_ty() -> Expr {
    Expr::App {
        func: Box::new(Expr::Free("List".into())),
        arg: Box::new(Expr::Free("Int".into())),
    }
}

/// A selector for a variant (`Cons`/`Nil`) of the `(List Int)` type.
fn list_variant(name: &str) -> Expr {
    Expr::Ctr {
        ty: Box::new(list_ty()),
        variant: Some(name.into()),
    }
}

/// `[]` — the empty list, i.e. `(List Int)::Nil`.
fn nil() -> Expr {
    list_variant("Nil")
}

/// `Cons head tail` == `App(App((List Int)::Cons, head), tail)`.
fn cons(head: Expr, tail: Expr) -> Expr {
    let app = |f, a| Expr::App {
        func: Box::new(f),
        arg: Box::new(a),
    };
    app(app(list_variant("Cons"), head), tail)
}

fn pat_key(pat: &Pattern) -> Result<Pat, String> {
    Ok(match pat {
        Pattern::Ctr(name) => Pat::Ctr(name.to_string()),
        Pattern::Nil => Pat::Ctr("Nil".into()),
        Pattern::Cons => Pat::Ctr("Cons".into()),
        Pattern::Lit(lit) => Pat::Val(lit_value(lit)),
        Pattern::Default => return Err("`_` pattern is handled as the default".into()),
    })
}

// --- occurrence counting (for affine checks and auto-dup arity) ---

fn binder_binds(b: &Binding, name: &str) -> bool {
    match b {
        Binding::Hole => false,
        Binding::Var { name: n, .. } => *n == name,
        Binding::Dup { names } => names.contains(&name),
    }
}

fn binders_bind(bs: &[Binding], name: &str) -> bool {
    bs.iter().any(|b| binder_binds(b, name))
}

/// Count uses of `name` across the remaining lambda binders + body.
fn count_in_rest(rest: &[Binding], body: &Node, name: &str) -> usize {
    if binders_bind(rest, name) {
        0
    } else {
        count_node(body, name)
    }
}

/// Count uses of `name` across the remaining let bindings + body.
fn count_seq(bindings: &[(Binding, Node)], body: &Node, name: &str) -> usize {
    let mut total = 0;
    for (b, val) in bindings {
        total += count_node(val, name);
        if binder_binds(b, name) {
            return total;
        }
    }
    total + count_node(body, name)
}

fn count_node(node: &Node, name: &str) -> usize {
    match node {
        Node::Var { name: n } => (*n == name) as usize,
        Node::Lit { .. } | Node::Wild | Node::Erase | Node::Primitive { .. } => 0,
        Node::List { elems } => elems.iter().map(|e| count_node(e, name)).sum(),
        Node::Sup { nodes, .. } => nodes.iter().map(|e| count_node(e, name)).sum(),
        Node::ProductType { fields } => fields.iter().map(|e| count_node(e, name)).sum(),
        Node::SumType { variants } => variants
            .iter()
            .map(|(_, args)| args.iter().map(|e| count_node(e, name)).sum::<usize>())
            .sum(),
        Node::Ctr { ty, .. } => count_node(ty, name),
        Node::App { func, args } => {
            count_node(func, name) + args.iter().map(|e| count_node(e, name)).sum::<usize>()
        }
        Node::Infix { left, right, .. } => count_node(left, name) + count_node(right, name),
        Node::Unary { expr, .. } => count_node(expr, name),
        Node::Lambda { binders, body } => {
            if binders_bind(binders, name) {
                0
            } else {
                count_node(body, name)
            }
        }
        Node::Let { bindings, body } => count_seq(bindings, body, name),
        Node::Match { cases, default } => {
            cases
                .iter()
                .map(|(_, t)| count_node(t, name))
                .sum::<usize>()
                + default.as_ref().map_or(0, |d| count_node(d, name))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn de(src: &str) -> Expr {
        let node = crate::core::parse::parse(src).unwrap();
        desugar(&node).unwrap()
    }

    fn lam(body: Expr) -> Expr {
        Expr::Lam {
            body: Box::new(body),
        }
    }
    fn use_(body: Expr) -> Expr {
        Expr::Use {
            body: Box::new(body),
        }
    }
    fn app(f: Expr, a: Expr) -> Expr {
        Expr::App {
            func: Box::new(f),
            arg: Box::new(a),
        }
    }

    #[test]
    fn identity_de_bruijn() {
        assert_eq!(de(r"\x -> x"), lam(Expr::Var(DeBruijn(0))));
        // K = \x y -> x : y is unused, so the inner lambda becomes an erasing
        // `Use`; the body still refers to the outer binder (index 1).
        assert_eq!(de(r"\x y -> x"), lam(use_(Expr::Var(DeBruijn(1)))));
    }

    #[test]
    fn auto_dup_is_explicit() {
        // \&x -> x x  becomes an explicit dup over the lambda's argument
        assert_eq!(
            de(r"\&x -> x x"),
            lam(Expr::Dup {
                val: Box::new(Expr::Var(DeBruijn(0))),
                body: Box::new(app(Expr::Ref(DeBruijn(0)), Expr::Ref(DeBruijn(0)))),
            })
        );
    }

    #[test]
    fn cloned_let_builds_single_dup() {
        // `&x = 1; x + x` shares one value and duplicates it across its two uses
        // with a single dup; each use is a distinct projection (`Ref`).
        assert_eq!(
            de(r"&x = 1; x + x"),
            Expr::Dup {
                val: Box::new(Expr::Value(Value::Int(1))),
                body: Box::new(Expr::Bop {
                    op: BinaryOp::Add,
                    left: Box::new(Expr::Ref(DeBruijn(0))),
                    right: Box::new(Expr::Ref(DeBruijn(0))),
                }),
            }
        );
    }

    #[test]
    fn single_use_let_is_inlined() {
        // A single use needs no dup: the value is inlined at its use site, whether
        // or not the binder is marked `&`.
        assert_eq!(de(r"x = 1; x + 2"), de(r"1 + 2"));
        assert_eq!(de(r"&x = 1; x + 2"), de(r"1 + 2"));
    }

    #[test]
    fn match_field_binders_desugar_to_lambda() {
        // `?{Con x -> body}` is sugar for `?{Con -> \x -> body}`.
        assert_eq!(
            de(r"?{Con x -> x; [] -> 0}"),
            de(r"?{Con -> \x -> x; [] -> 0}")
        );
        // multiple binders nest lambdas.
        assert_eq!(
            de(r"?{Con h t -> h; [] -> 0}"),
            de(r"?{Con -> \h t -> h; [] -> 0}")
        );
    }

    #[test]
    fn match_underscore_is_default() {
        // `_ -> 2` routes to the default slot rather than becoming a case.
        assert_eq!(
            de(r"?{X -> 1; _ -> 2}"),
            Expr::Mat {
                cases: vec![(Pat::Ctr("X".into()), Expr::Value(Value::Int(1)))],
                default: Some(Box::new(Expr::Value(Value::Int(2)))),
            }
        );
    }

    #[test]
    fn match_duplicate_default_errors() {
        // two `_ ->` branches are an ambiguous double default.
        let node = crate::core::parse::parse(r"?{X -> 1; _ -> 2; _ -> 3}").unwrap();
        assert!(desugar(&node).is_err());
    }
}
