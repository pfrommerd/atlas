//! Lowering: the atlas surface AST -> the desugared core IR
//! ([`atlas_core::core::expr::Expr`]).
//!
//! Unlike the core surface language, atlas binders carry no affine / auto-dup /
//! erased annotation. The classification is inferred here from the number of
//! uses of each binder in its scope:
//!
//! - 0 uses: an erasing binder ([`Expr::Use`] for lambdas; `let` values are
//!   dropped entirely),
//! - 1 use: a plain linear binder ([`Expr::Lam`] + [`Expr::Var`]; `let` values
//!   are inlined at their one use site),
//! - `N >= 2` uses: a binary dup chain of `N-1` [`Expr::Dup`] levels, with use
//!   sites lowered to [`Expr::Dp0`] / [`Expr::Dp1`] projections.
//!
//! Recursive bindings are not allowed, with one exception: `fn foo(..) { .. }`
//! binds `foo` within its own body. When the body actually uses `foo`, the
//! declaration lowers to a closed Y-combinator applied to `\foo -> \args.. ->
//! body`; when it does not, it lowers to a plain lambda chain. General
//! recursion (e.g. mutual recursion, recursive `let`) is deliberately not
//! supported yet.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use atlas_core::core::expr::{DeBruijn, Expr, Pat, TypeDefKind, Value};
use atlas_core::vm::term as vm;
use ordered_float::OrderedFloat;

use crate::ast;

// ========================================================================
// Public entry points
// ========================================================================

/// Lower a closed expression: unbound names are an error.
pub fn lower_expr(e: &ast::Expr) -> Result<Expr, String> {
    let no_ctors = HashMap::new();
    Lower::new(false, &no_ctors).expr(e)
}

/// Lower an open expression: unbound names lower to [`Expr::Free`] (resolved
/// later, e.g. against a REPL's locals). `ctors` maps variant names to the
/// (free) name of the enum binding they construct, seeded from enum
/// declarations submitted earlier in the session.
pub fn lower_expr_open<'a>(
    e: &'a ast::Expr<'a>,
    ctors: &'a HashMap<String, String>,
) -> Result<Expr, String> {
    Lower::new(true, ctors).expr(e)
}

/// A lowered top-level declaration (a REPL line or one item of a `.at` module):
/// the name to bind, its open-lowered value, and — for enum declarations — the
/// variant names that should resolve to this binding from now on.
#[derive(Debug, Clone, PartialEq)]
pub struct LoweredDecl {
    pub name: String,
    pub expr: Expr,
    pub variants: Vec<String>,
}

/// Lower a top-level declaration (open, like [`lower_expr_open`]). Returns
/// `Ok(None)` for a `let _ = ..;` (the value is dropped). Top-level
/// redefinitions are ordinary shadowing, so no shadow checks apply here — the
/// caller just overwrites its binding and ctor maps.
pub fn lower_decl_open<'a>(
    d: &'a ast::Declaration<'a>,
    ctors: &'a HashMap<String, String>,
) -> Result<Option<LoweredDecl>, String> {
    let mut l = Lower::new(true, ctors);
    match d {
        ast::Declaration::Let(decl) => match &decl.pattern {
            ast::Pattern::Identifier(name) => Ok(Some(LoweredDecl {
                name: (*name).to_string(),
                expr: l.expr(&decl.value)?,
                variants: Vec::new(),
            })),
            ast::Pattern::Wildcard => Ok(None),
            p => Err(format!("unsupported `let` pattern {p:?} in lowering")),
        },
        ast::Declaration::Fn(f) => Ok(Some(LoweredDecl {
            name: f.name.to_string(),
            expr: l.fn_value(f)?,
            variants: Vec::new(),
        })),
        ast::Declaration::Enum(en) => Ok(Some(LoweredDecl {
            name: en.name.to_string(),
            expr: l.enum_value(en)?,
            variants: en
                .variants
                .iter()
                .map(|v| variant_name(v).to_string())
                .collect(),
        })),
        d => Err(format!(
            "`{}` declarations are not yet supported in lowering",
            decl_kind(d)
        )),
    }
}

// ========================================================================
// The lowering context
// ========================================================================

/// What a bound name resolves to during lowering.
#[derive(Clone)]
enum Binding<'a> {
    /// a `Lam` binder bound at this absolute depth (exactly one use)
    Lam(usize),
    /// a binder with `>= 2` uses, expanded into a binary dup chain
    Cloned(Rc<RefCell<DupState>>),
    /// a single-use `let`-like binding, re-lowered (inlined) at its use site
    Inline(InlineVal<'a>),
}

/// The (unlowered) value of an inlinable binding.
#[derive(Clone, Copy)]
enum InlineVal<'a> {
    Expr(&'a ast::Expr<'a>),
    Fn(&'a ast::FnDecl<'a>),
    Enum(&'a ast::EnumDecl<'a>),
}

struct DupState {
    /// absolute depths of the `N-1` dup binders in the chain
    dup_depths: Vec<usize>,
    /// total number of uses
    count: usize,
    /// uses consumed so far
    used: usize,
}

struct Lower<'a> {
    /// binder depth: every `Lam`, `Use`, and `Dup` level counts
    depth: usize,
    /// bound value names (lowercase) and enum type names (uppercase)
    env: HashMap<&'a str, Binding<'a>>,
    /// variant name -> the enum binder (or free name) it constructs
    ctors: HashMap<&'a str, &'a str>,
    /// when set, unbound names lower to [`Expr::Free`] rather than erroring
    open: bool,
}

impl<'a> Lower<'a> {
    fn new(open: bool, ctors: &'a HashMap<String, String>) -> Self {
        Lower {
            depth: 0,
            env: HashMap::new(),
            ctors: ctors
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str()))
                .collect(),
            open,
        }
    }

    // --- expressions ---

    fn expr(&mut self, e: &'a ast::Expr<'a>) -> Result<Expr, String> {
        match e {
            ast::Expr::Literal(l) => Ok(Expr::Value(lit_value(l)?)),
            ast::Expr::Identifier(name) => self.use_name(name),
            ast::Expr::Constructor(c) => self.constructor(c),
            ast::Expr::List(l) => {
                let mut acc = nil();
                for e in l.elems.iter().rev() {
                    let head = self.expr(e)?;
                    acc = cons(head, acc);
                }
                Ok(acc)
            }
            ast::Expr::IfElse(b) => self.if_else(b),
            ast::Expr::Match(m) => self.match_expr(m),
            ast::Expr::Block(b) => self.block(b),
            ast::Expr::Unary { op, expr } => Ok(Expr::Uop {
                op: unary_op(*op),
                val: Box::new(self.expr(expr)?),
            }),
            ast::Expr::Infix { lhs, op, rhs } => Ok(Expr::Bop {
                op: infix_op(*op),
                left: Box::new(self.expr(lhs)?),
                right: Box::new(self.expr(rhs)?),
            }),
            ast::Expr::Call(f, args) => {
                let mut acc = self.expr(f)?;
                for arg in args {
                    let x = self.expr(arg)?;
                    acc = app(acc, x);
                }
                Ok(acc)
            }
            ast::Expr::Tuple(_) => Err("tuples are not yet supported in lowering".into()),
            ast::Expr::Project(..) => {
                Err("field projection is not yet supported in lowering".into())
            }
            ast::Expr::Scope(_) => Err("module paths are not yet supported in lowering".into()),
            ast::Expr::Index(..) => Err("indexing is not yet supported in lowering".into()),
        }
    }

    /// Resolve a use of a bound name, driving dup projection and let inlining.
    fn use_name(&mut self, name: &'a str) -> Result<Expr, String> {
        enum What<'a> {
            Lam(usize),
            Dup { dup_depth: usize, side: bool },
            Inline(InlineVal<'a>),
        }
        let what = match self.env.get(name) {
            None => {
                if self.open {
                    return Ok(Expr::Free(name.to_string()));
                }
                return Err(format!("unbound variable `{name}`"));
            }
            Some(Binding::Lam(d)) => What::Lam(*d),
            Some(Binding::Cloned(c)) => {
                let mut c = c.borrow_mut();
                let m = c.used;
                c.used += 1;
                // Each use selects one side of the binary dup chain: the m-th
                // use takes the first projection of the m-th dup, and the last
                // use takes the second projection of the final dup.
                if m < c.count - 1 {
                    What::Dup {
                        dup_depth: c.dup_depths[m],
                        side: false,
                    }
                } else {
                    What::Dup {
                        dup_depth: c.dup_depths[c.count - 2],
                        side: true,
                    }
                }
            }
            Some(Binding::Inline(v)) => What::Inline(*v),
        };
        let depth = self.depth;
        let idx = |d: usize| DeBruijn((depth - 1 - d) as u64);
        Ok(match what {
            What::Lam(d) => Expr::Var(idx(d)),
            What::Dup { dup_depth: d, side } => {
                if side {
                    Expr::Dp1(idx(d))
                } else {
                    Expr::Dp0(idx(d))
                }
            }
            What::Inline(v) => {
                // The bound value's scope excludes its own binder (recursion is
                // not allowed), so a mention of `name` inside the value must
                // resolve to the *outer* binding, not loop back to this one.
                let prev = self.env.remove(name);
                let r = self.inline_value(v);
                restore(&mut self.env, name, prev);
                r?
            }
        })
    }

    /// Lower the value of an inlinable binding at its (single) use site, or as
    /// the shared value of a dup chain.
    fn inline_value(&mut self, v: InlineVal<'a>) -> Result<Expr, String> {
        match v {
            InlineVal::Expr(e) => self.expr(e),
            InlineVal::Fn(f) => self.fn_value(f),
            InlineVal::Enum(en) => self.enum_value(en),
        }
    }

    fn constructor(&mut self, c: &'a ast::Constructor<'a>) -> Result<Expr, String> {
        let (name, args): (&'a str, &'a [ast::Expr<'a>]) = match c {
            ast::Constructor::Empty(name) => (name, &[]),
            ast::Constructor::Tuple(name, args) => (name, args.as_slice()),
            ast::Constructor::Struct(..) => {
                return Err("struct constructors are not yet supported in lowering".into());
            }
        };
        // Resolution: a bound (enum) name is the type value itself; a known
        // variant name selects the constructor of its enum's binding; anything
        // else is free (open mode) or unbound (closed mode).
        let ctor_ty = self.ctors.get(name).copied();
        let head = if self.env.contains_key(name) {
            self.use_name(name)?
        } else if let Some(ty_name) = ctor_ty {
            Expr::Ctr {
                ty: Box::new(self.use_name(ty_name)?),
                variant: Some(name.to_string()),
            }
        } else if self.open {
            Expr::Free(name.to_string())
        } else {
            return Err(format!("unbound constructor `{name}`"));
        };
        let mut acc = head;
        for arg in args {
            let x = self.expr(arg)?;
            acc = app(acc, x);
        }
        Ok(acc)
    }

    /// `if cond { a } else { b }` is a bool match applied to the condition; the
    /// default arm erases the (false) scrutinee.
    fn if_else(&mut self, b: &'a ast::IfElse<'a>) -> Result<Expr, String> {
        let cond = self.expr(&b.cond)?;
        let then = self.expr(&b.if_expr)?;
        self.depth += 1; // the default's erasing binder
        let els = self.expr(&b.else_expr);
        self.depth -= 1;
        Ok(Expr::App {
            func: Box::new(Expr::Mat {
                cases: vec![(Pat::Val(Value::Bool(true)), then)],
                default: Some(Box::new(Expr::Use {
                    body: Box::new(els?),
                })),
            }),
            arg: Box::new(cond),
        })
    }

    fn match_expr(&mut self, m: &'a ast::Match<'a>) -> Result<Expr, String> {
        let scrut = self.expr(&m.scrut)?;
        let mut cases = Vec::new();
        let mut default: Option<Expr> = None;
        for arm in &m.arms {
            match &arm.pattern {
                ast::Pattern::Literal(l) => {
                    cases.push((Pat::Val(lit_value(l)?), self.expr(&arm.body)?));
                }
                ast::Pattern::Constructor(name, subpats) => {
                    let body = self.ctor_fields(subpats, 0, &arm.body)?;
                    cases.push((Pat::Ctr((*name).to_string()), body));
                }
                // An identifier arm is the default: a lambda binding the whole
                // scrutinee (the value that failed every case).
                ast::Pattern::Identifier(name) => {
                    if default.is_some() {
                        return Err("match has more than one default branch".into());
                    }
                    let n = self.count_expr(&arm.body, name);
                    default = Some(self.lam_binder(name, n, |s| s.expr(&arm.body))?);
                }
                ast::Pattern::Wildcard => {
                    if default.is_some() {
                        return Err("match has more than one default branch".into());
                    }
                    self.depth += 1;
                    let body = self.expr(&arm.body);
                    self.depth -= 1;
                    default = Some(Expr::Use {
                        body: Box::new(body?),
                    });
                }
                p => return Err(format!("unsupported match pattern {p:?} in lowering")),
            }
        }
        Ok(Expr::App {
            func: Box::new(Expr::Mat {
                cases,
                default: default.map(Box::new),
            }),
            arg: Box::new(scrut),
        })
    }

    /// A constructor arm's body is a lambda over the constructor's fields, one
    /// binder per sub-pattern.
    fn ctor_fields(
        &mut self,
        subpats: &'a [ast::Pattern<'a>],
        idx: usize,
        body: &'a ast::Expr<'a>,
    ) -> Result<Expr, String> {
        let pat = match subpats.get(idx) {
            None => return self.expr(body),
            Some(p) => p,
        };
        match pat {
            ast::Pattern::Identifier(name) => {
                let n = if pats_bind(&subpats[idx + 1..], name) {
                    0
                } else {
                    self.count_expr(body, name)
                };
                self.lam_binder(name, n, |s| s.ctor_fields(subpats, idx + 1, body))
            }
            ast::Pattern::Wildcard => {
                self.depth += 1;
                let inner = self.ctor_fields(subpats, idx + 1, body);
                self.depth -= 1;
                Ok(Expr::Use {
                    body: Box::new(inner?),
                })
            }
            p => Err(format!(
                "nested pattern {p:?} is not yet supported in lowering"
            )),
        }
    }

    // --- blocks and declarations ---

    fn block(&mut self, b: &'a ast::ExprBlock<'a>) -> Result<Expr, String> {
        self.decls(&b.decls, 0, b.value.as_ref())
    }

    fn decls(
        &mut self,
        decls: &'a [ast::Declaration<'a>],
        idx: usize,
        value: Option<&'a ast::Expr<'a>>,
    ) -> Result<Expr, String> {
        let decl = match decls.get(idx) {
            None => {
                return match value {
                    Some(e) => self.expr(e),
                    None => Err("a block must end in an expression".into()),
                };
            }
            Some(d) => d,
        };
        match decl {
            ast::Declaration::Let(l) => match &l.pattern {
                ast::Pattern::Identifier(name) => {
                    let n = self.count_decls(&decls[idx + 1..], value, name);
                    self.let_binder(name, n, InlineVal::Expr(&l.value), |s| {
                        s.decls(decls, idx + 1, value)
                    })
                }
                // erased let: drop the value
                ast::Pattern::Wildcard => self.decls(decls, idx + 1, value),
                p => Err(format!("unsupported `let` pattern {p:?} in lowering")),
            },
            ast::Declaration::Fn(f) => {
                let n = self.count_decls(&decls[idx + 1..], value, f.name);
                self.let_binder(f.name, n, InlineVal::Fn(f), |s| {
                    s.decls(decls, idx + 1, value)
                })
            }
            ast::Declaration::Enum(en) => {
                self.check_enum_shadowing(en)?;
                // The variants resolve to this enum binding for the rest of the
                // block (including any inline re-lowering of later bindings).
                let prevs: Vec<_> = en
                    .variants
                    .iter()
                    .map(|v| {
                        let name = variant_name(v);
                        (name, self.ctors.insert(name, en.name))
                    })
                    .collect();
                let n = self.count_decls(&decls[idx + 1..], value, en.name);
                let r = self.let_binder(en.name, n, InlineVal::Enum(en), |s| {
                    s.decls(decls, idx + 1, value)
                });
                for (name, prev) in prevs.into_iter().rev() {
                    restore(&mut self.ctors, name, prev);
                }
                r
            }
            d => Err(format!(
                "`{}` declarations are not yet supported in lowering",
                decl_kind(d)
            )),
        }
    }

    /// A single ctor map keyed by variant name can't express shadowing, so
    /// redefining a live enum or variant name inside an expression is rejected
    /// (first pass). Top-level (REPL) redefinition is fine — see
    /// [`lower_decl_open`].
    fn check_enum_shadowing(&self, en: &ast::EnumDecl) -> Result<(), String> {
        if self.env.contains_key(en.name) || self.ctors.values().any(|&v| v == en.name) {
            return Err(format!(
                "enum `{}` shadows an existing binding; shadowing enums is not yet supported",
                en.name
            ));
        }
        for v in &en.variants {
            let name = variant_name(v);
            if self.ctors.contains_key(name) || self.env.contains_key(name) {
                return Err(format!(
                    "variant `{name}` shadows an existing binding; shadowing variants is not yet supported"
                ));
            }
        }
        Ok(())
    }

    // --- fn and enum values ---

    /// The value of a `fn` declaration: a lambda chain over its arguments. The
    /// function's own name is bound within its body; if the body actually uses
    /// it, the whole chain is wrapped in `\foo -> ..` and applied to the
    /// Y-combinator (the sole form of recursion supported so far).
    fn fn_value(&mut self, f: &'a ast::FnDecl<'a>) -> Result<Expr, String> {
        let rec = if f.args.iter().any(|p| pat_binds(p, f.name)) {
            0
        } else {
            self.count_block(&f.body, f.name)
        };
        if rec == 0 {
            return self.fn_args(f, 0);
        }
        let wrapped = self.lam_binder(f.name, rec, |s| s.fn_args(f, 0))?;
        Ok(app(y_combinator(), wrapped))
    }

    fn fn_args(&mut self, f: &'a ast::FnDecl<'a>, idx: usize) -> Result<Expr, String> {
        let pat = match f.args.get(idx) {
            None => return self.block(&f.body),
            Some(p) => p,
        };
        match pat {
            ast::Pattern::Identifier(name) => {
                let n = if pats_bind(&f.args[idx + 1..], name) {
                    0
                } else {
                    self.count_block(&f.body, name)
                };
                self.lam_binder(name, n, |s| s.fn_args(f, idx + 1))
            }
            ast::Pattern::Wildcard => {
                self.depth += 1;
                let inner = self.fn_args(f, idx + 1);
                self.depth -= 1;
                Ok(Expr::Use {
                    body: Box::new(inner?),
                })
            }
            p => Err(format!("unsupported fn argument pattern {p:?} in lowering")),
        }
    }

    fn enum_value(&mut self, en: &'a ast::EnumDecl<'a>) -> Result<Expr, String> {
        if en.variants.is_empty() {
            return Err(format!("enum `{}` must have at least one variant", en.name));
        }
        let mut variants = Vec::with_capacity(en.variants.len());
        for v in &en.variants {
            let (name, args) = match v {
                ast::EnumVariant::Empty(name) => (*name, Vec::new()),
                ast::EnumVariant::Tuple(name, tys) => {
                    let mut args = Vec::with_capacity(tys.len());
                    for t in tys {
                        args.push(self.lower_type(t)?);
                    }
                    (*name, args)
                }
                ast::EnumVariant::Struct(..) => {
                    return Err("struct enum variants are not yet supported in lowering".into());
                }
            };
            if name == "New" {
                return Err(
                    "`New` is reserved as the product constructor and cannot be used as a variant name"
                        .into(),
                );
            }
            variants.push((name.to_string(), args));
        }
        Ok(Expr::TypeDef {
            kind: TypeDefKind::Sum(variants),
        })
    }

    fn lower_type(&mut self, t: &'a ast::Type<'a>) -> Result<Expr, String> {
        match t {
            ast::Type::Identifier(name) => self.use_name(name),
        }
    }

    // --- binder machinery ---

    /// Emit a lambda binder for `name` with `n` precomputed uses in the body
    /// produced by `f`: an erasing `Use` (0), a plain `Lam` (1), or a `Lam`
    /// over an `n-1`-long binary dup chain (>= 2).
    fn lam_binder(
        &mut self,
        name: &'a str,
        n: usize,
        f: impl FnOnce(&mut Self) -> Result<Expr, String>,
    ) -> Result<Expr, String> {
        if n == 0 {
            self.depth += 1;
            let inner = f(self);
            self.depth -= 1;
            return Ok(Expr::Use {
                body: Box::new(inner?),
            });
        }
        if n == 1 {
            let lam_depth = self.depth;
            self.depth += 1;
            let prev = self.env.insert(name, Binding::Lam(lam_depth));
            let inner = f(self);
            restore(&mut self.env, name, prev);
            self.depth -= 1;
            return Ok(Expr::Lam {
                body: Box::new(inner?),
            });
        }
        // n >= 2: the lambda's own binder plus the dup chain over its argument.
        self.depth += 1;
        let base = self.depth;
        let dup_depths: Vec<usize> = (0..n - 1).map(|j| base + j).collect();
        self.depth += dup_depths.len();
        let state = Rc::new(RefCell::new(DupState {
            dup_depths: dup_depths.clone(),
            count: n,
            used: 0,
        }));
        let prev = self.env.insert(name, Binding::Cloned(state));
        let inner = f(self);
        restore(&mut self.env, name, prev);
        self.depth -= 1 + dup_depths.len();

        let mut e = inner?;
        for j in (0..dup_depths.len()).rev() {
            let val = if j == 0 {
                Expr::Var(DeBruijn(0)) // the lambda's argument
            } else {
                Expr::Dp1(DeBruijn(0)) // fed by the enclosing dup
            };
            e = Expr::Dup {
                val: Box::new(val),
                body: Box::new(e),
            };
        }
        Ok(Expr::Lam { body: Box::new(e) })
    }

    /// Emit a `let`-like binder for `name` with `n` precomputed uses in the
    /// rest of the scope produced by `f`: dropped (0), inlined at the one use
    /// site (1), or lowered once and shared through an `n-1` dup chain (>= 2).
    fn let_binder(
        &mut self,
        name: &'a str,
        n: usize,
        val: InlineVal<'a>,
        f: impl FnOnce(&mut Self) -> Result<Expr, String>,
    ) -> Result<Expr, String> {
        if n == 0 {
            return f(self);
        }
        if n == 1 {
            let prev = self.env.insert(name, Binding::Inline(val));
            let r = f(self);
            restore(&mut self.env, name, prev);
            return r;
        }
        // n >= 2: the value is duplicated, not re-lowered, so lower it once
        // here (in the scope outside the chain's dup binders).
        let val_expr = self.inline_value(val)?;
        let base = self.depth;
        let dup_depths: Vec<usize> = (0..n - 1).map(|j| base + j).collect();
        self.depth += dup_depths.len();
        let state = Rc::new(RefCell::new(DupState {
            dup_depths: dup_depths.clone(),
            count: n,
            used: 0,
        }));
        let prev = self.env.insert(name, Binding::Cloned(state));
        let inner = f(self);
        restore(&mut self.env, name, prev);
        self.depth -= dup_depths.len();

        let mut e = inner?;
        let mut val_expr = Some(val_expr);
        for j in (0..dup_depths.len()).rev() {
            let val = if j == 0 {
                val_expr.take().unwrap()
            } else {
                Expr::Dp1(DeBruijn(0))
            };
            e = Expr::Dup {
                val: Box::new(val),
                body: Box::new(e),
            };
        }
        Ok(e)
    }

    // --- occurrence counting ---
    //
    // Counting runs over the *surface* AST before a binder is emitted, so the
    // totals must agree exactly with the number of `use_name` resolutions the
    // emission pass performs. Shadowing binders stop the count for the region
    // they cover.

    fn count_expr(&self, e: &ast::Expr, name: &str) -> usize {
        match e {
            ast::Expr::Literal(_) | ast::Expr::Scope(_) => 0,
            ast::Expr::Identifier(n) => (*n == name) as usize,
            ast::Expr::Constructor(c) => match c {
                ast::Constructor::Empty(n) => self.ctor_counts(n, name),
                ast::Constructor::Tuple(n, args) => {
                    self.ctor_counts(n, name)
                        + args.iter().map(|a| self.count_expr(a, name)).sum::<usize>()
                }
                ast::Constructor::Struct(n, fields) => {
                    self.ctor_counts(n, name)
                        + fields
                            .iter()
                            .map(|(_, e)| self.count_expr(e, name))
                            .sum::<usize>()
                }
            },
            ast::Expr::Tuple(t) => t.fields.iter().map(|e| self.count_expr(e, name)).sum(),
            ast::Expr::List(l) => l.elems.iter().map(|e| self.count_expr(e, name)).sum(),
            ast::Expr::IfElse(b) => {
                self.count_expr(&b.cond, name)
                    + self.count_expr(&b.if_expr, name)
                    + self.count_expr(&b.else_expr, name)
            }
            ast::Expr::Match(m) => {
                self.count_expr(&m.scrut, name)
                    + m.arms
                        .iter()
                        .map(|arm| {
                            if pat_binds(&arm.pattern, name) {
                                0
                            } else {
                                self.count_expr(&arm.body, name)
                            }
                        })
                        .sum::<usize>()
            }
            ast::Expr::Block(b) => self.count_block(b, name),
            ast::Expr::Unary { expr, .. } => self.count_expr(expr, name),
            ast::Expr::Infix { lhs, rhs, .. } => {
                self.count_expr(lhs, name) + self.count_expr(rhs, name)
            }
            ast::Expr::Project(e, _) => self.count_expr(e, name),
            ast::Expr::Index(e, i) => self.count_expr(e, name) + self.count_expr(i, name),
            ast::Expr::Call(f, args) => {
                self.count_expr(f, name)
                    + args.iter().map(|a| self.count_expr(a, name)).sum::<usize>()
            }
        }
    }

    /// How many uses of the binder `name` one constructor-head mention of `n`
    /// contributes: a direct mention of the enum binding itself, or a variant
    /// that resolves to it (a `Ctr` whose `ty` uses the binding).
    fn ctor_counts(&self, n: &str, name: &str) -> usize {
        (n == name || self.ctors.get(n).is_some_and(|&ty| ty == name)) as usize
    }

    fn count_block(&self, b: &ast::ExprBlock, name: &str) -> usize {
        self.count_decls(&b.decls, b.value.as_ref(), name)
    }

    /// Count uses of `name` across a run of declarations and the trailing block
    /// value, stopping when a declaration rebinds `name`.
    fn count_decls(
        &self,
        decls: &[ast::Declaration],
        value: Option<&ast::Expr>,
        name: &str,
    ) -> usize {
        let mut total = 0;
        for d in decls {
            match d {
                ast::Declaration::Let(l) => {
                    total += self.count_expr(&l.value, name);
                    if pat_binds(&l.pattern, name) {
                        return total;
                    }
                }
                ast::Declaration::Fn(f) => {
                    total += self.count_fn(f, name);
                    if f.name == name {
                        return total;
                    }
                }
                ast::Declaration::Enum(en) => {
                    total += self.count_enum(en, name);
                    if en.name == name {
                        return total;
                    }
                }
                // Unsupported declarations error during emission; count nothing.
                _ => {}
            }
        }
        total + value.map_or(0, |v| self.count_expr(v, name))
    }

    /// Uses of an *outer* `name` inside a `fn` value. The function's own name
    /// and its argument binders shadow.
    fn count_fn(&self, f: &ast::FnDecl, name: &str) -> usize {
        if f.name == name || f.args.iter().any(|p| pat_binds(p, name)) {
            0
        } else {
            self.count_block(&f.body, name)
        }
    }

    /// Uses of `name` in an enum declaration's variant field types.
    fn count_enum(&self, en: &ast::EnumDecl, name: &str) -> usize {
        en.variants
            .iter()
            .map(|v| match v {
                ast::EnumVariant::Tuple(_, tys) => tys
                    .iter()
                    .filter(|t| matches!(t, ast::Type::Identifier(n) if *n == name))
                    .count(),
                ast::EnumVariant::Struct(_, fields) => fields
                    .iter()
                    .filter(|(_, t)| matches!(t, ast::Type::Identifier(n) if *n == name))
                    .count(),
                ast::EnumVariant::Empty(_) => 0,
            })
            .sum()
    }
}

// ========================================================================
// Helpers
// ========================================================================

fn restore<'a, V>(map: &mut HashMap<&'a str, V>, name: &'a str, prev: Option<V>) {
    match prev {
        Some(v) => {
            map.insert(name, v);
        }
        None => {
            map.remove(name);
        }
    }
}

fn pat_binds(p: &ast::Pattern, name: &str) -> bool {
    match p {
        ast::Pattern::Identifier(n) => *n == name,
        ast::Pattern::Wildcard | ast::Pattern::Literal(_) => false,
        ast::Pattern::Constructor(_, subs) => pats_bind(subs, name),
        ast::Pattern::Tuple(subs) => pats_bind(subs, name),
        ast::Pattern::Typed(inner, _) => pat_binds(inner, name),
    }
}

fn pats_bind(ps: &[ast::Pattern], name: &str) -> bool {
    ps.iter().any(|p| pat_binds(p, name))
}

fn variant_name<'a>(v: &ast::EnumVariant<'a>) -> &'a str {
    match v {
        ast::EnumVariant::Tuple(n, _)
        | ast::EnumVariant::Struct(n, _)
        | ast::EnumVariant::Empty(n) => n,
    }
}

fn decl_kind(d: &ast::Declaration) -> &'static str {
    match d {
        ast::Declaration::Mod(_) => "mod",
        ast::Declaration::Let(_) => "let",
        ast::Declaration::Fn(_) => "fn",
        ast::Declaration::Alias(_) => "type",
        ast::Declaration::Enum(_) => "enum",
        ast::Declaration::Struct(_) => "struct",
        ast::Declaration::Trait(_) => "trait",
        ast::Declaration::Impl(_) => "impl",
    }
}

fn lit_value(l: &ast::Literal) -> Result<Value, String> {
    Ok(match l {
        ast::Literal::Integer(i) => Value::Int(*i),
        ast::Literal::Float(x) => Value::Float(OrderedFloat(x.into_inner())),
        ast::Literal::Bool(b) => Value::Bool(*b),
        ast::Literal::String(s) => Value::Str((*s).to_string()),
        ast::Literal::Unit => return Err("`()` is not yet supported in lowering".into()),
    })
}

fn infix_op(op: ast::InfixOp) -> vm::BinaryOp {
    use ast::InfixOp as I;
    use vm::BinaryOp as B;
    match op {
        I::Add => B::Add,
        I::Sub => B::Sub,
        I::Mul => B::Mul,
        I::Div => B::Div,
        I::Mod => B::Mod,
        I::Eq => B::Eq,
        I::Neq => B::Neq,
        I::Lt => B::Lt,
        I::Lte => B::Lte,
        I::Gt => B::Gt,
        I::Gte => B::Gte,
        I::And => B::And,
        I::Or => B::Or,
        I::Xor => B::Xor,
        I::Shl => B::Shl,
        I::Shr => B::Shr,
    }
}

fn unary_op(op: ast::UnaryOp) -> vm::UnaryOp {
    match op {
        ast::UnaryOp::Neg => vm::UnaryOp::Neg,
        ast::UnaryOp::Not => vm::UnaryOp::Not,
    }
}

fn app(func: Expr, arg: Expr) -> Expr {
    Expr::App {
        func: Box::new(func),
        arg: Box::new(arg),
    }
}

/// The list element type used by list sugar: `(List Int)`, matching core's list
/// desugaring. `List` and `Int` are free names resolved later by the prelude.
fn list_variant(name: &str) -> Expr {
    Expr::Ctr {
        ty: Box::new(app(Expr::Free("List".into()), Expr::Free("Int".into()))),
        variant: Some(name.into()),
    }
}

fn nil() -> Expr {
    list_variant("Nil")
}

fn cons(head: Expr, tail: Expr) -> Expr {
    app(app(list_variant("Cons"), head), tail)
}

/// The Y-combinator `\&f -> (\&x -> f (x x)) (\&x -> f (x x))` as a closed
/// core expression, identical to what core's desugarer produces for `fix`
/// (verified by a test below): binder levels outside-in are `Lam f` (0),
/// `Dup f` (1), and within each inner lambda `Lam x` (2) and `Dup x` (3), so
/// use sites at depth 4 see `f`'s dup at index 2 and `x`'s at index 0.
fn y_combinator() -> Expr {
    let inner = |f_use: Expr| Expr::Lam {
        body: Box::new(Expr::Dup {
            val: Box::new(Expr::Var(DeBruijn(0))),
            body: Box::new(app(
                f_use,
                app(Expr::Dp0(DeBruijn(0)), Expr::Dp1(DeBruijn(0))),
            )),
        }),
    };
    Expr::Lam {
        body: Box::new(Expr::Dup {
            val: Box::new(Expr::Var(DeBruijn(0))),
            body: Box::new(app(
                inner(Expr::Dp0(DeBruijn(2))),
                inner(Expr::Dp1(DeBruijn(2))),
            )),
        }),
    }
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{parse_expr, parse_repl};

    /// Lower a closed expression from source.
    fn de(src: &str) -> Expr {
        let e = parse_expr(src).unwrap_or_else(|m| panic!("parse error in {src:?}: {m}"));
        lower_expr(&e).unwrap_or_else(|m| panic!("lowering error in {src:?}: {m}"))
    }

    fn de_err(src: &str) -> String {
        let e = parse_expr(src).unwrap_or_else(|m| panic!("parse error in {src:?}: {m}"));
        lower_expr(&e).expect_err(&format!("expected a lowering error for {src:?}"))
    }

    fn de_open(src: &str) -> Expr {
        let e = parse_expr(src).unwrap_or_else(|m| panic!("parse error in {src:?}: {m}"));
        lower_expr_open(&e, &HashMap::new())
            .unwrap_or_else(|m| panic!("lowering error in {src:?}: {m}"))
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
    fn dup(val: Expr, body: Expr) -> Expr {
        Expr::Dup {
            val: Box::new(val),
            body: Box::new(body),
        }
    }
    fn int(i: i64) -> Expr {
        Expr::Value(Value::Int(i))
    }
    fn bop(op: vm::BinaryOp, l: Expr, r: Expr) -> Expr {
        Expr::Bop {
            op,
            left: Box::new(l),
            right: Box::new(r),
        }
    }
    fn var(i: u64) -> Expr {
        Expr::Var(DeBruijn(i))
    }
    fn dp0(i: u64) -> Expr {
        Expr::Dp0(DeBruijn(i))
    }
    fn dp1(i: u64) -> Expr {
        Expr::Dp1(DeBruijn(i))
    }

    #[test]
    fn literals() {
        assert_eq!(de("1"), int(1));
        assert_eq!(de("true"), Expr::Value(Value::Bool(true)));
        assert_eq!(de("1 + 2"), bop(vm::BinaryOp::Add, int(1), int(2)));
    }

    #[test]
    fn single_use_let_inlines() {
        assert_eq!(de("let x = 1 in x + 2"), de("1 + 2"));
    }

    #[test]
    fn unused_let_drops_value() {
        assert_eq!(de("let x = 1 in 2"), int(2));
        assert_eq!(de("let _ = 1 in 2"), int(2));
    }

    #[test]
    fn double_use_let_dups() {
        assert_eq!(
            de("let x = 1 in x + x"),
            dup(int(1), bop(vm::BinaryOp::Add, dp0(0), dp1(0)))
        );
    }

    #[test]
    fn triple_use_let_chains_dups() {
        assert_eq!(
            de("let x = 1 in x + (x + x)"),
            dup(
                int(1),
                dup(
                    dp1(0),
                    bop(
                        vm::BinaryOp::Add,
                        dp0(1),
                        bop(vm::BinaryOp::Add, dp0(0), dp1(0))
                    )
                )
            )
        );
    }

    #[test]
    fn fn_binders_classify_by_use_count() {
        // unused arg: an erasing binder
        assert_eq!(de("let f x = 1 in f"), use_(int(1)));
        // single use: a plain linear binder
        assert_eq!(de("let f x = x in f"), lam(var(0)));
        // double use: a dup chain over the argument
        assert_eq!(
            de("let f x = x + x in f"),
            lam(dup(var(0), bop(vm::BinaryOp::Add, dp0(0), dp1(0))))
        );
    }

    #[test]
    fn multi_arg_fn_nests_lambdas() {
        assert_eq!(
            de("let add a b = a + b in add"),
            lam(lam(bop(vm::BinaryOp::Add, var(1), var(0))))
        );
        assert_eq!(de("let add a b = a + b in add 1 2"), {
            app(
                app(lam(lam(bop(vm::BinaryOp::Add, var(1), var(0)))), int(1)),
                int(2),
            )
        });
    }

    #[test]
    fn fn_used_twice_dups_the_lambda() {
        assert_eq!(
            de("let id x = x in id (id 1)"),
            dup(lam(var(0)), app(dp0(0), app(dp1(0), int(1))))
        );
    }

    #[test]
    fn if_else_is_a_bool_match() {
        assert_eq!(
            de("if true { 1 } else { 2 }"),
            Expr::App {
                func: Box::new(Expr::Mat {
                    cases: vec![(Pat::Val(Value::Bool(true)), int(1))],
                    default: Some(Box::new(use_(int(2)))),
                }),
                arg: Box::new(Expr::Value(Value::Bool(true))),
            }
        );
    }

    #[test]
    fn match_lowers_cases_and_defaults() {
        // literal case + wildcard default
        assert_eq!(
            de("match 1 { 1 => 10, _ => 20 }"),
            Expr::App {
                func: Box::new(Expr::Mat {
                    cases: vec![(Pat::Val(Value::Int(1)), int(10))],
                    default: Some(Box::new(use_(int(20)))),
                }),
                arg: Box::new(int(1)),
            }
        );
        // identifier default binds the scrutinee
        assert_eq!(
            de("match 1 { 2 => 0, x => x + 1 }"),
            Expr::App {
                func: Box::new(Expr::Mat {
                    cases: vec![(Pat::Val(Value::Int(2)), int(0))],
                    default: Some(Box::new(lam(bop(vm::BinaryOp::Add, var(0), int(1))))),
                }),
                arg: Box::new(int(1)),
            }
        );
        assert!(de_err("match 1 { x => x, _ => 2 }").contains("more than one default"));
    }

    #[test]
    fn ctor_match_arm_binds_fields() {
        let e = de_open("match v { Some(x) => x + 1, None => 0 }");
        let expected = Expr::App {
            func: Box::new(Expr::Mat {
                cases: vec![
                    (
                        Pat::Ctr("Some".into()),
                        lam(bop(vm::BinaryOp::Add, var(0), int(1))),
                    ),
                    (Pat::Ctr("None".into()), int(0)),
                ],
                default: None,
            }),
            arg: Box::new(Expr::Free("v".into())),
        };
        assert_eq!(e, expected);
    }

    #[test]
    fn enum_decl_binds_type_and_variants() {
        let color = Expr::TypeDef {
            kind: TypeDefKind::Sum(vec![("Red".into(), vec![]), ("Green".into(), vec![])]),
        };
        // A single variant use inlines the type value into the Ctr.
        assert_eq!(
            de("{\nenum Color { Red, Green }\nRed\n}"),
            Expr::Ctr {
                ty: Box::new(color.clone()),
                variant: Some("Red".into()),
            }
        );
        // Two uses share one lowered TypeDef through a dup.
        assert_eq!(
            de("{\nenum Color { Red, Green }\nif true { Red } else { Green }\n}"),
            Expr::App {
                func: Box::new(Expr::Mat {
                    cases: vec![(
                        Pat::Val(Value::Bool(true)),
                        Expr::Ctr {
                            ty: Box::new(dp0(0)),
                            variant: Some("Red".into()),
                        }
                    )],
                    default: Some(Box::new(use_(Expr::Ctr {
                        ty: Box::new(dp1(1)),
                        variant: Some("Green".into()),
                    }))),
                }),
                arg: Box::new(Expr::Value(Value::Bool(true))),
            }
            .pipe(|body| dup(color.clone(), body))
        );
        // The bare enum name is the type value itself.
        assert_eq!(de("{\nenum Color { Red }\nColor\n}"), color_single());
        // Variant argument types resolve as free names.
        assert_eq!(
            de_open("{\nenum Opt { None, Some(Int) }\nOpt\n}"),
            Expr::TypeDef {
                kind: TypeDefKind::Sum(vec![
                    ("None".into(), vec![]),
                    ("Some".into(), vec![Expr::Free("Int".into())]),
                ]),
            }
        );
    }

    fn color_single() -> Expr {
        Expr::TypeDef {
            kind: TypeDefKind::Sum(vec![("Red".into(), vec![])]),
        }
    }

    // tiny pipe helper so expected shapes read outside-in
    trait Pipe: Sized {
        fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
            f(self)
        }
    }
    impl Pipe for Expr {}

    #[test]
    fn ctor_args_apply() {
        assert_eq!(
            de("{\nenum Opt { None, Some }\nSome(1)\n}"),
            app(
                Expr::Ctr {
                    ty: Box::new(Expr::TypeDef {
                        kind: TypeDefKind::Sum(vec![
                            ("None".into(), vec![]),
                            ("Some".into(), vec![]),
                        ]),
                    }),
                    variant: Some("Some".into()),
                },
                int(1)
            )
        );
    }

    #[test]
    fn unknown_names_free_when_open_error_when_closed() {
        assert_eq!(de_open("foo"), Expr::Free("foo".into()));
        assert_eq!(de_open("Foo"), Expr::Free("Foo".into()));
        assert_eq!(de_open("Foo(1)"), app(Expr::Free("Foo".into()), int(1)));
        assert!(de_err("foo").contains("unbound variable"));
        assert!(de_err("Foo").contains("unbound constructor"));
    }

    #[test]
    fn seeded_ctors_resolve_to_free_enum() {
        let mut ctors = HashMap::new();
        ctors.insert("Red".to_string(), "Color".to_string());
        let e = parse_expr("Red").unwrap();
        assert_eq!(
            lower_expr_open(&e, &ctors).unwrap(),
            Expr::Ctr {
                ty: Box::new(Expr::Free("Color".into())),
                variant: Some("Red".into()),
            }
        );
    }

    #[test]
    fn list_literal_matches_core_desugaring() {
        let core_node = atlas_core::core::parse::parse("[1, 2]").unwrap();
        let core_expr = atlas_core::core::ast::desugar_open(&core_node).unwrap();
        assert_eq!(de_open("[1, 2]"), core_expr);
    }

    #[test]
    fn y_combinator_matches_core_fix() {
        let fix = atlas_core::core::ast::desugar(&atlas_core::core::ast::Node::Fix).unwrap();
        assert_eq!(y_combinator(), fix);
    }

    #[test]
    fn recursive_fn_wraps_in_y_combinator() {
        // `f` used once in its own body: Y applied to \f -> \n -> f n
        assert_eq!(
            de("let f n = f n in f"),
            app(y_combinator(), lam(lam(app(var(1), var(0)))))
        );
        // non-recursive fn: no Y
        assert_eq!(de("let f n = n in f"), lam(var(0)));
    }

    #[test]
    fn recursive_let_is_not_allowed() {
        assert!(de_err("let x = x in x").contains("unbound"));
    }

    #[test]
    fn shadowing_enums_rejected() {
        assert!(de_err("{\nenum Color { Red }\nenum Color { Blue }\nColor\n}").contains("shadow"));
        assert!(de_err("{\nenum A { Red }\nenum B { Red }\nRed\n}").contains("shadow"));
    }

    #[test]
    fn unsupported_constructs_error() {
        assert!(de_err("(1, 2)").contains("tuples"));
        assert!(de_err("foo.bar").contains("projection"));
        assert!(de_err("()").contains("()"));
        assert!(de_err("{\nstruct P { x: Int }\n1\n}").contains("struct"));
        assert!(de_err("{\nlet x = 1\n}").contains("block must end"));
    }

    #[test]
    fn repl_decls_lower_open() {
        let ctors = HashMap::new();
        fn parse_decl(src: &str) -> crate::ast::Declaration<'_> {
            match parse_repl(src).unwrap() {
                crate::ast::ReplInput::Declaration(d) => d,
                other => panic!("expected a declaration, got {other:?}"),
            }
        }

        let d = parse_decl("add a b = a + b");
        let lowered = lower_decl_open(&d, &ctors).unwrap().unwrap();
        assert_eq!(lowered.name, "add");
        assert_eq!(
            lowered.expr,
            lam(lam(bop(vm::BinaryOp::Add, var(1), var(0))))
        );
        assert!(lowered.variants.is_empty());

        let d = parse_decl("enum Color { Red, Green }");
        let lowered = lower_decl_open(&d, &ctors).unwrap().unwrap();
        assert_eq!(lowered.name, "Color");
        assert_eq!(
            lowered.variants,
            vec!["Red".to_string(), "Green".to_string()]
        );

        let d = parse_decl("let _ = 1");
        assert!(lower_decl_open(&d, &ctors).unwrap().is_none());
    }
}
