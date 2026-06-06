use std::collections::HashMap;

use crate::core::expr::{DeBruijn, Expr, Label, Pat};
use crate::vm::term::BinaryOp;

#[rustfmt::skip]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfixOp {
    Add, Sub, Mul,
    Div, Mod,
    And, Or, Xor,
    Shl, Shr, Eq, Neq,
    Lt, Lte, Gt, Gte, Cons,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal<'src> {
    Integer(u64),
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
    // &Label{a, b, c} for explicit dup
    Dup {
        label: &'src str,
        names: Vec<&'src str>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[rustfmt::skip]
pub enum Node<'src> {
    // literal
    Lit { val: Literal<'src> },
    // List literal [node, node, ...]
    // gets desugared to #Con{node, #Con{node, #Con{node, #Nil}}}
    List { elems: Vec<Node<'src>> },
    /// variable: Foo or Foo#0 or Foo#1 for dup variables
    Var { name: &'src str },
    // reference: @Foo to a name in the book
    Ref { name: &'src str },
    // builtin primitive: %Foo
    Primitive { name: &'src str },
    /// superposition: `"&" Label "{" (Node ",")* Node "}"`
    Sup { label: &'src str, nodes: Vec<Node<'src>> },
    /// duplication term:
    // ! x = y; a + b
    Let { bindings: Vec<(Binding<'src>, Node<'src>)>, body: Box<Node<'src>>, },
    // \ &x -> x + x
    Lambda { binders: Vec<Binding<'src>>, body: Box<Node<'src>>, },
    /// erasure: `"&{}"` or equivalently, `\{}`
    Erase,
    /// constructor: `"#" Name "{" Node,* "}"`
    Construct { name: &'src str, args: Vec<Node<'src>>, },
    /// pattern match: `"?""{" (Pattern "->" Node ";")* Term "}"`
    /// note that ?{Term} or ?{_ -> Term} applies the unboxed value to Term
    /// i.e. ?{\x -> x} #Some{1} ==> 1
    Match { cases: Vec<(Pattern<'src>, Node<'src>)>, default: Option<Box<Node<'src>>> },
    /// f a
    App { func: Box<Node<'src>>, args: Vec<Node<'src>>, },
    /// infix operation: Node Oper Term
    Infix { left: Box<Node<'src>>, op: InfixOp, right: Box<Node<'src>>, },
    /// wildcard: `*`
    Wild,
}

// ========================================================================
// Lowering: surface AST (`Node`) -> desugared core IR (`Expr`)
// ========================================================================

/// Lower a surface AST node into a desugared [`Expr`].
pub fn desugar<'n>(node: &'n Node<'n>) -> Result<Expr, String> {
    let mut d = Desugar {
        auto: 0,
        depth: 0,
        env: HashMap::new(),
    };
    d.go(node)
}

/// What a source name resolves to during desugaring.
enum BindingDesugar<'n> {
    /// a `Lam` binder bound at this absolute depth
    Lam(usize),
    /// a `Dup` projection bound at this absolute depth (`false` = `Dp0`)
    DupSide(usize, bool),
    /// a cloned (`&x`) lambda binder, expanded into an explicit dup chain
    Cloned(DupDesugar),
    /// a let binding, inlined (re-desugared) at every use
    Let(&'n Node<'n>),
}

/// State for a cloned binder's auto-duplication chain.
struct DupDesugar {
    /// absolute depth of the original `Lam` binder
    lam_depth: usize,
    /// absolute depths of the `N-1` dup binders in the chain
    dup_depths: Vec<usize>,
    /// total number of uses
    count: usize,
    /// uses consumed so far
    used: usize,
}

struct Desugar<'n> {
    auto: u32,
    depth: usize,
    env: HashMap<&'n str, BindingDesugar<'n>>,
}

impl<'n> Desugar<'n> {
    fn fresh_label(&mut self) -> Label {
        let n = self.auto;
        self.auto += 1;
        Label::Auto(n)
    }

    fn go(&mut self, node: &'n Node<'n>) -> Result<Expr, String> {
        match node {
            Node::Lit { val } => Ok(self.lit(val)),
            Node::List { elems } => self.list(elems),
            Node::Var { name } => self.use_var(name),
            Node::Ref { name } => Ok(Expr::Ref(name.to_string())),
            Node::Primitive { name } => Ok(Expr::Pri(name.to_string())),
            Node::Wild => Ok(Expr::Wld),
            Node::Erase => Ok(Expr::Era),
            Node::Sup { label, nodes } => {
                if nodes.len() != 2 {
                    return Err("superposition must have exactly two elements".into());
                }
                let left = self.go(&nodes[0])?;
                let right = self.go(&nodes[1])?;
                Ok(Expr::Sup {
                    label: Label::Named(label.to_string()),
                    left: Box::new(left),
                    right: Box::new(right),
                })
            }
            Node::Construct { name, args } => {
                if args.len() >= 16 {
                    return Err("constructor arity must be < 16".into());
                }
                let mut a = Vec::with_capacity(args.len());
                for arg in args {
                    a.push(self.go(arg)?);
                }
                Ok(Expr::Ctr {
                    name: name.to_string(),
                    args: a,
                })
            }
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
                    return Ok(Expr::Ctr {
                        name: "Con".into(),
                        args: vec![l, r],
                    });
                }
                Ok(Expr::Op2 {
                    op: op.try_into().expect("Unexpected Cons in Op2!"),
                    left: Box::new(l),
                    right: Box::new(r),
                })
            }
            Node::Lambda { binders, body } => self.lam(binders, body),
            Node::Let { bindings, body } => self.lets(bindings, 0, body),
            Node::Match { cases, default } => {
                let mut compiled = Vec::with_capacity(cases.len());
                for (pat, body) in cases {
                    compiled.push((pat_key(pat)?, self.go(body)?));
                }
                let default = match default {
                    Some(d) => Some(Box::new(self.go(d)?)),
                    None => None,
                };
                Ok(Expr::Mat {
                    cases: compiled,
                    default,
                })
            }
        }
    }

    fn lit(&mut self, lit: &Literal) -> Expr {
        match lit {
            Literal::Integer(i) => Expr::Num(*i),
            Literal::Char(c) => chr(*c as u64),
            Literal::String(s) => {
                let mut acc = Expr::Ctr {
                    name: "Nil".into(),
                    args: vec![],
                };
                for c in s.chars().rev() {
                    acc = Expr::Ctr {
                        name: "Con".into(),
                        args: vec![chr(c as u64), acc],
                    };
                }
                acc
            }
        }
    }

    fn list(&mut self, elems: &'n [Node<'n>]) -> Result<Expr, String> {
        let mut acc = Expr::Ctr {
            name: "Nil".into(),
            args: vec![],
        };
        for e in elems.iter().rev() {
            let head = self.go(e)?;
            acc = Expr::Ctr {
                name: "Con".into(),
                args: vec![head, acc],
            };
        }
        Ok(acc)
    }

    /// Resolve a variable use (de Bruijn), driving auto-dup and let inlining.
    fn use_var(&mut self, name: &'n str) -> Result<Expr, String> {
        enum What<'n> {
            Lam(usize),
            DupSide(usize, bool),
            ClonedSingle(usize), // lam_depth (count <= 1)
            ClonedDup { dup_depth: usize, side: bool },
            Inline(&'n Node<'n>),
        }
        let what = match self.env.get(name) {
            None => return Err(format!("unbound variable `{name}`")),
            Some(BindingDesugar::Lam(d)) => What::Lam(*d),
            Some(BindingDesugar::DupSide(d, s)) => What::DupSide(*d, *s),
            Some(BindingDesugar::Let(n)) => What::Inline(*n),
            Some(BindingDesugar::Cloned(c)) => {
                if c.count <= 1 {
                    What::ClonedSingle(c.lam_depth)
                } else {
                    let m = c.used;
                    if m < c.count - 1 {
                        What::ClonedDup {
                            dup_depth: c.dup_depths[m],
                            side: false,
                        }
                    } else {
                        What::ClonedDup {
                            dup_depth: c.dup_depths[c.count - 2],
                            side: true,
                        }
                    }
                }
            }
        };
        if let Some(BindingDesugar::Cloned(c)) = self.env.get_mut(name) {
            c.used += 1;
        }
        let depth = self.depth;
        let idx = |d: usize| DeBruijn((depth - 1 - d) as u64);
        Ok(match what {
            What::Lam(d) => Expr::Var(idx(d)),
            What::ClonedSingle(d) => Expr::Var(idx(d)),
            What::DupSide(d, side) => {
                if side {
                    Expr::Dp1(idx(d))
                } else {
                    Expr::Dp0(idx(d))
                }
            }
            What::ClonedDup { dup_depth, side } => {
                if side {
                    Expr::Dp1(idx(dup_depth))
                } else {
                    Expr::Dp0(idx(dup_depth))
                }
            }
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
                Ok(Expr::Lam {
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
                let lam_depth = self.depth;
                self.depth += 1;
                let prev = self.env.insert(name, BindingDesugar::Lam(lam_depth));
                let inner = self.lam(rest, body);
                restore(&mut self.env, name, prev);
                self.depth -= 1;
                Ok(Expr::Lam {
                    body: Box::new(inner?),
                })
            }
            Binding::Var { name, auto_dup: true } => {
                let count = count_in_rest(rest, body, name);
                let lam_depth = self.depth;
                self.depth += 1; // the lambda's own binder

                let dup_depths: Vec<usize> = if count >= 2 {
                    (0..count - 1).map(|j| lam_depth + 1 + j).collect()
                } else {
                    Vec::new()
                };
                self.depth += dup_depths.len();

                let prev = self.env.insert(
                    name,
                    BindingDesugar::Cloned(DupDesugar {
                        lam_depth,
                        dup_depths: dup_depths.clone(),
                        count,
                        used: 0,
                    }),
                );
                let inner = self.lam(rest, body);
                restore(&mut self.env, name, prev);
                self.depth -= dup_depths.len();
                self.depth -= 1;

                let mut e = inner?;
                // wrap the dup chain (innermost dup first)
                for j in (0..dup_depths.len()).rev() {
                    let val = if j == 0 {
                        Expr::Var(DeBruijn(0))
                    } else {
                        Expr::Dp1(DeBruijn(0))
                    };
                    e = Expr::Dup {
                        label: self.fresh_label(),
                        val: Box::new(val),
                        body: Box::new(e),
                    };
                }
                Ok(Expr::Lam { body: Box::new(e) })
            }
            Binding::Dup { label, names } => {
                if names.len() != 2 {
                    return Err("lambda dup binder must bind exactly two names".into());
                }
                self.depth += 1; // the lambda's own (anonymous) binder
                let dup_depth = self.depth;
                self.depth += 1;
                let p0 = self
                    .env
                    .insert(names[0], BindingDesugar::DupSide(dup_depth, false));
                let p1 = self
                    .env
                    .insert(names[1], BindingDesugar::DupSide(dup_depth, true));
                let inner = self.lam(rest, body);
                restore(&mut self.env, names[1], p1);
                restore(&mut self.env, names[0], p0);
                self.depth -= 2;

                let inner = inner?;
                let dup = Expr::Dup {
                    label: Label::Named(label.to_string()),
                    val: Box::new(Expr::Var(DeBruijn(0))), // the lambda's argument
                    body: Box::new(inner),
                };
                Ok(Expr::Lam {
                    body: Box::new(dup),
                })
            }
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
            } => {
                // a cloned let of a (shared) term: re-instantiate fresh per use
                let prev = self.env.insert(name, BindingDesugar::Let(val));
                let r = self.lets(bindings, idx + 1, body);
                restore(&mut self.env, name, prev);
                r
            }
            Binding::Dup { label, names } => {
                if names.len() != 2 {
                    return Err("dup binder must bind exactly two names".into());
                }
                let val_expr = self.go(val)?;
                let dup_depth = self.depth;
                self.depth += 1;
                let p0 = self
                    .env
                    .insert(names[0], BindingDesugar::DupSide(dup_depth, false));
                let p1 = self
                    .env
                    .insert(names[1], BindingDesugar::DupSide(dup_depth, true));
                let inner = self.lets(bindings, idx + 1, body);
                restore(&mut self.env, names[1], p1);
                restore(&mut self.env, names[0], p0);
                self.depth -= 1;
                Ok(Expr::Dup {
                    label: Label::Named(label.to_string()),
                    val: Box::new(val_expr),
                    body: Box::new(inner?),
                })
            }
        }
    }
}

fn chr(code: u64) -> Expr {
    Expr::Ctr {
        name: "Chr".into(),
        args: vec![Expr::Num(code)],
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

fn pat_key(pat: &Pattern) -> Result<Pat, String> {
    Ok(match pat {
        Pattern::Ctr(name) => Pat::Ctr(name.to_string()),
        Pattern::Nil => Pat::Ctr("Nil".into()),
        Pattern::Cons => Pat::Ctr("Con".into()),
        Pattern::Lit(Literal::Integer(i)) => Pat::Num(*i),
        Pattern::Lit(_) => return Err("only integer literal patterns are supported".into()),
        Pattern::Default => return Err("`_` pattern is handled as the default".into()),
    })
}

// --- occurrence counting (for affine checks and auto-dup arity) ---

fn binder_binds(b: &Binding, name: &str) -> bool {
    match b {
        Binding::Hole => false,
        Binding::Var { name: n, .. } => *n == name,
        Binding::Dup { names, .. } => names.iter().any(|n| *n == name),
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
        Node::Lit { .. } | Node::Wild | Node::Erase | Node::Ref { .. } | Node::Primitive { .. } => {
            0
        }
        Node::List { elems } => elems.iter().map(|e| count_node(e, name)).sum(),
        Node::Sup { nodes, .. } => nodes.iter().map(|e| count_node(e, name)).sum(),
        Node::Construct { args, .. } => args.iter().map(|e| count_node(e, name)).sum(),
        Node::App { func, args } => {
            count_node(func, name) + args.iter().map(|e| count_node(e, name)).sum::<usize>()
        }
        Node::Infix { left, right, .. } => count_node(left, name) + count_node(right, name),
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
    fn app(f: Expr, a: Expr) -> Expr {
        Expr::App {
            func: Box::new(f),
            arg: Box::new(a),
        }
    }

    #[test]
    fn identity_de_bruijn() {
        assert_eq!(de(r"\x -> x"), lam(Expr::Var(DeBruijn(0))));
        // K = \x y -> x : the inner body refers to the outer binder (index 1)
        assert_eq!(de(r"\x y -> x"), lam(lam(Expr::Var(DeBruijn(1)))));
    }

    #[test]
    fn auto_dup_is_explicit() {
        // \&x -> x x  becomes an explicit dup over the lambda's argument
        assert_eq!(
            de(r"\&x -> x x"),
            lam(Expr::Dup {
                label: Label::Auto(0),
                val: Box::new(Expr::Var(DeBruijn(0))),
                body: Box::new(app(Expr::Dp0(DeBruijn(0)), Expr::Dp1(DeBruijn(0)))),
            })
        );
    }

    #[test]
    fn list_desugars_to_ctrs() {
        assert_eq!(
            de(r"[1, 2]"),
            Expr::Ctr {
                name: "Con".into(),
                args: vec![
                    Expr::Num(1),
                    Expr::Ctr {
                        name: "Con".into(),
                        args: vec![
                            Expr::Num(2),
                            Expr::Ctr {
                                name: "Nil".into(),
                                args: vec![]
                            }
                        ],
                    },
                ],
            }
        );
    }
}
