pub mod engine;
pub mod exec;
pub mod heap;
pub mod memory;
pub mod term;

use std::cell::Cell;
use std::fmt;

use crate::core::ast;
use crate::util::memo_map::MemoMap;
use exec::{Executor, Extensions, FiniteBudget, NoExtensions};
use heap::Heap;
use memory::{CtrPtr, DupPtr, NodePtr};
use term::{Node, Term};

/// Default interaction budget for [`run`].
pub const DEFAULT_BUDGET: u64 = 50_000_000;

/// Parse, desugar, evaluate, and pretty-print a single source expression,
/// resolving any primitives (`%name`) through `ext`.
pub fn run_with<X: Extensions>(src: &str, ext: X) -> Result<String, String> {
    let node = crate::core::parse::parse(src)?;
    let expr = ast::desugar(&node)?;
    let mut heap = Heap::new();
    let root = heap.lower_with(&expr, &ext)?;
    // Drive the async engine on a single-threaded runtime (deterministic, and
    // enough for awaiting async primitives). Scope the executor so its `&mut
    // heap` borrow ends before printing; recover `ext` for the printer's names.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let (root, ext) = {
        let mut exec = Executor::with_extensions(&mut heap, FiniteBudget::new(DEFAULT_BUDGET), ext);
        let root = runtime.block_on(exec.eval_normalize(root));
        (root, exec.extensions)
    };
    Ok(format!("{}", Printer::with_extensions(&heap, &ext).pretty(root)))
}

/// Parse, desugar, evaluate, and pretty-print a single source expression.
/// Programs that use primitives (`%name`) should call [`run_with`] instead.
pub fn run(src: &str) -> Result<String, String> {
    run_with(src, NoExtensions)
}

// ========================================================================
// Readback / printing
// ========================================================================

pub struct Printer<'a, X: Extensions = NoExtensions> {
    heap: &'a Heap,
    extensions: &'a X,
    var_names: MemoMap<NodePtr, String>,
    dup_names: MemoMap<DupPtr, String>,
    name_counter: Cell<usize>,
}

/// A [`Node`] paired with the [`Printer`] that knows how to render it; the
/// [`Display`](fmt::Display) impl forwards to [`Printer::fmt`].
pub struct PrettyNode<'a, X: Extensions = NoExtensions> {
    printer: &'a Printer<'a, X>,
    target: Node,
}

impl<X: Extensions> fmt::Display for PrettyNode<'_, X> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.printer.fmt(f, self.target)
    }
}

impl<'a> Printer<'a, NoExtensions> {
    /// A printer that renders primitives by their numeric id (no name lookup).
    pub fn new(heap: &'a Heap) -> Self {
        const NO_EXT: &NoExtensions = &NoExtensions;
        Printer::with_extensions(heap, NO_EXT)
    }
}

impl<'a, X: Extensions> Printer<'a, X> {
    /// A printer that resolves primitive names through `extensions`.
    pub fn with_extensions(heap: &'a Heap, extensions: &'a X) -> Self {
        Printer {
            heap,
            extensions,
            var_names: MemoMap::new(),
            dup_names: MemoMap::new(),
            name_counter: Cell::new(0),
        }
    }

    pub fn pretty<'s>(&'s self, target: Node) -> PrettyNode<'s, X> {
        PrettyNode {
            printer: self,
            target,
        }
    }

    fn fresh_name(&self) -> String {
        let n = self.name_counter.get();
        self.name_counter.set(n + 1);
        let letter = (b'a' + (n % 26) as u8) as char;
        if n < 26 {
            letter.to_string()
        } else {
            format!("{}{}", letter, n / 26)
        }
    }

    /// Name of the binder at `binder_loc`, allocating a fresh one on first use.
    fn var_name(&self, binder_loc: NodePtr) -> &str {
        self.var_names
            .get_or_insert_with(binder_loc, || self.fresh_name())
    }

    /// Name of the duplication node `d`, allocating a fresh one on first use.
    fn dup_name(&self, d: DupPtr) -> &str {
        self.dup_names.get_or_insert_with(d, || self.fresh_name())
    }

    /// Render `t` into `f`, recursing through the heap. Mirrors the
    /// [`Display`](fmt::Display) trait's `fmt`, but threads the target node.
    pub fn fmt(&self, f: &mut fmt::Formatter<'_>, t: Node) -> fmt::Result {
        self.fmt_prec(f, t, true)
    }

    /// Render `t` into `f`. `tail` is true when `t` occupies a position where an
    /// unparenthesized lambda is unambiguous — the top level or the body of an
    /// enclosing lambda, i.e. nowhere else can a token follow it. Everywhere
    /// else (function/argument of an application, an operand, a field) a lambda
    /// would greedily swallow what comes after, so it must be parenthesized.
    /// Every other form is already self-delimiting and ignores `tail`.
    fn fmt_prec(&self, f: &mut fmt::Formatter<'_>, t: Node, tail: bool) -> fmt::Result {
        match t.unpack() {
            Term::Lam(p) => {
                if !tail {
                    write!(f, "(")?;
                }
                write!(f, "\\{} -> ", self.var_name(NodePtr(p.0)))?;
                let (_, body) = self.heap.pair(p);
                self.fmt_prec(f, body, true)?;
                if !tail {
                    write!(f, ")")?;
                }
                Ok(())
            }
            Term::App(p) => {
                let (func, arg) = self.heap.pair(p);
                write!(f, "(")?;
                self.fmt_prec(f, func, false)?;
                write!(f, " ")?;
                self.fmt_prec(f, arg, false)?;
                write!(f, ")")
            }
            Term::Var(p) => match self.heap.node(p).unpack() {
                Term::Sub(n) => self.fmt_prec(f, n, tail),
                _ => write!(f, "{}", self.var_name(p)),
            },
            Term::Dp0(q) => self.fmt_dup(f, q, q.sub0(), "0", tail),
            Term::Dp1(q) => self.fmt_dup(f, q, q.sub1(), "1", tail),
            Term::Sup(p) => {
                let lab = self.heap.label(self.heap.sup_label(p));
                let (a, b) = self.heap.sup_args(p);
                write!(f, "&{}{{", lab)?;
                self.fmt_prec(f, a, false)?;
                write!(f, ", ")?;
                self.fmt_prec(f, b, false)?;
                write!(f, "}}")
            }
            Term::Num(n) => write!(f, "{}", n),
            Term::Pri(id) => match self.extensions.name(id) {
                Some(name) => write!(f, "%{}", name),
                None => write!(f, "%{}", id.0),
            },
            Term::Ctr(base) => self.fmt_ctr(f, base),
            Term::Use(v) => {
                if !tail {
                    write!(f, "(")?;
                }
                write!(f, "\\_ -> ")?;
                self.fmt_prec(f, self.heap.node(v), true)?;
                if !tail {
                    write!(f, ")")?;
                }
                Ok(())
            }
            Term::Wld => write!(f, "_"),
            Term::Era => write!(f, "*"),
            Term::Bop(p) => {
                let op = self.heap.node(p.first()).as_op();
                let (l, r) = (self.heap.node(p.second()), self.heap.node(p.third()));
                write!(f, "(")?;
                self.fmt_prec(f, l, false)?;
                write!(f, " {} ", op.symbol())?;
                self.fmt_prec(f, r, false)?;
                write!(f, ")")
            }
            Term::Mat(_) => write!(f, "?{{...}}"),
            _ => write!(f, "<?>"),
        }
    }

    /// A free (unsubstituted) duplication projection, or its substitution.
    fn fmt_dup(
        &self,
        f: &mut fmt::Formatter<'_>,
        dp: DupPtr,
        slot: NodePtr,
        suffix: &str,
        tail: bool,
    ) -> fmt::Result {
        match self.heap.node(slot).unpack() {
            Term::Sub(n) => self.fmt_prec(f, n, tail),
            _ => write!(f, "{}.{}", self.dup_name(dp), suffix),
        }
    }

    fn fmt_ctr(&self, f: &mut fmt::Formatter<'_>, base: CtrPtr) -> fmt::Result {
        let (name, arity) = self.heap.ctr_head(base);
        let nm = self.heap.name(name);
        let arity = arity.0;
        // list sugar
        if nm == "Nil" && arity == 0 {
            return write!(f, "[]");
        }
        if nm == "Con" && arity == 2 {
            write!(f, "[")?;
            let mut cell = base;
            let mut first = true;
            loop {
                if !first {
                    write!(f, ", ")?;
                }
                first = false;
                let head = self.heap.node(self.heap.ctr_field(cell, 0));
                self.fmt_prec(f, head, false)?;
                let tail = self.heap.node(self.heap.ctr_field(cell, 1));
                match tail.unpack() {
                    Term::Ctr(b)
                        if {
                            let (n, a) = self.heap.ctr_head(b);
                            self.heap.name(n) == "Con" && a.0 == 2
                        } =>
                    {
                        cell = b;
                    }
                    Term::Ctr(b) if self.heap.name(self.heap.ctr_head(b).0) == "Nil" => {
                        return write!(f, "]");
                    }
                    _ => {
                        // improper list: fall back
                        write!(f, ", ")?;
                        self.fmt_prec(f, tail, false)?;
                        return write!(f, "]");
                    }
                }
            }
        }
        if arity == 0 {
            write!(f, "#{}", nm)
        } else {
            write!(f, "#{}{{", nm)?;
            for i in 0..arity {
                if i > 0 {
                    write!(f, ", ")?;
                }
                let field = self.heap.node(self.heap.ctr_field(base, i));
                self.fmt_prec(f, field, false)?;
            }
            write!(f, "}}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::exec::{PrimFuture, PrimResult};
    use crate::vm::term::PrimId;
    use std::borrow::Cow;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn eval(src: &str) -> String {
        run(src).unwrap_or_else(|e| panic!("eval `{src}` failed: {e}"))
    }

    /// A tiny extension set exercising primitives: `%inc` (arity 1) and `%add`
    /// (arity 2). Both force their (raw) arguments through the executor.
    #[derive(Clone, Copy)]
    struct Arith;

    const INC: PrimId = PrimId(0);
    const ADD: PrimId = PrimId(1);

    impl Extensions for Arith {
        fn resolve(&self, name: &str) -> Option<PrimId> {
            match name {
                "inc" => Some(INC),
                "add" => Some(ADD),
                _ => None,
            }
        }
        fn arity(&self, id: PrimId) -> usize {
            match id {
                INC => 1,
                ADD => 2,
                _ => 0,
            }
        }
        fn name(&self, id: PrimId) -> Option<Cow<'_, str>> {
            match id {
                INC => Some("inc".into()),
                ADD => Some("add".into()),
                _ => None,
            }
        }
        fn apply(&self, heap: &mut Heap, id: PrimId, args: &[Node]) -> PrimResult {
            // Args arrive already in WHNF (forced by the engine).
            let num = |arg: Node| -> Option<u64> {
                match arg.unpack() {
                    Term::Num(n) => Some(n),
                    _ => None,
                }
            };
            let node = match id {
                INC => match num(args[0]) {
                    Some(n) => heap.num(n + 1),
                    None => heap.era(),
                },
                ADD => match (num(args[0]), num(args[1])) {
                    (Some(a), Some(b)) => heap.num(a + b),
                    _ => heap.era(),
                },
                _ => heap.era(),
            };
            PrimResult::Done(node)
        }
    }

    fn eval_ext(src: &str) -> String {
        run_with(src, Arith).unwrap_or_else(|e| panic!("eval `{src}` failed: {e}"))
    }

    #[test]
    fn primitive_inc() {
        assert_eq!(eval_ext("%inc 4"), "5");
    }

    #[test]
    fn primitive_forces_raw_args() {
        // arguments arrive unevaluated; the primitive reduces them itself
        assert_eq!(eval_ext("%add (2 + 3) (%inc 9)"), "15");
    }

    #[test]
    fn primitive_partial_application_is_value() {
        // under-applied: stays an inert value, printed with its resolved name
        assert_eq!(eval_ext("%add 1"), "(%add 1)");
    }

    #[test]
    fn primitive_over_application() {
        // %inc 4 => 5, then (5 6) is a stuck application of a number
        assert_eq!(eval_ext("%inc 4 6"), "(5 6)");
    }

    #[test]
    fn primitive_duplicates_to_itself() {
        // a cloned binder duplicates the primitive (DUP-PRI) for each use
        assert_eq!(eval_ext(r"(\&f -> [f 1, f 2]) %inc"), "[2, 3]");
    }

    #[test]
    fn unknown_primitive_is_rejected() {
        assert!(run_with("%nope 1", Arith).is_err());
        // and with no extensions at all, every primitive is unknown
        assert!(run("%inc 1").is_err());
    }

    fn eval_budget(src: &str, budget: u64) -> (String, u64) {
        let node = crate::core::parse::parse(src).unwrap();
        let expr = ast::desugar(&node).unwrap();
        let mut heap = Heap::new();
        let root = heap.lower(&expr).unwrap();
        let mut exec = Executor::new(&mut heap, FiniteBudget::new(budget));
        let root = exec.normalize(root);
        let itrs = exec.policy.interactions();
        (format!("{}", Printer::new(&heap).pretty(root)), itrs)
    }

    #[test]
    fn identity() {
        assert_eq!(eval(r"\x -> x"), r"\a -> a");
    }

    #[test]
    fn apply_identity() {
        assert_eq!(eval(r"(\x -> x) (\y -> y)"), r"\a -> a");
    }

    #[test]
    fn const_k() {
        // K applied to one argument: \x y -> x  given id  =>  \y -> id
        assert_eq!(eval(r"(\x y -> x) (\a -> a)"), r"\_ -> \a -> a");
    }

    #[test]
    fn arithmetic() {
        // note: `*` is the wildcard atom in this grammar, not multiply
        assert_eq!(eval(r"2 + 3 + 4"), "9");
        assert_eq!(eval(r"10 - 3"), "7");
    }

    #[test]
    fn let_binding() {
        assert_eq!(eval(r"x = 42; x"), "42");
        assert_eq!(eval(r"f = \x -> x; f 7"), "7");
    }

    #[test]
    fn cloned_binder_double() {
        // \&x -> x + x  applied to 5  => 10
        assert_eq!(eval(r"(\&x -> x + x) 5"), "10");
    }

    #[test]
    fn dup_sup_extract() {
        // explicit dup over a same-label sup annihilates pairwise
        assert_eq!(eval(r"&L{a b} = &L{1, 2}; [a, b]"), "[1, 2]");
    }

    #[test]
    fn church_two_squared() {
        // 2^2 = 4 ; church numeral applied to itself
        let c2 = r"\&s z -> s (s z)";
        let src = format!(r"&two = {c2}; two two");
        // 4 = \s z -> s (s (s (s z)))
        let (out, itrs) = eval_budget(&src, DEFAULT_BUDGET);
        assert_eq!(out, r"\a -> \b -> (a (a (a (a b))))");
        // should finish in a small number of interactions (optimal-ish)
        assert!(itrs < 100, "took {itrs} interactions");
    }

    #[test]
    fn church_add() {
        // add = \n m s z -> n s (m s z); add 1 2 = 3
        let one = r"\s z -> s z";
        let two = r"\&s z -> s (s z)";
        let src = format!(r"add = \n m &s z -> n s (m s z); add ({one}) ({two})");
        assert_eq!(eval(&src), r"\a -> \b -> (a (a (a b)))");
    }

    #[test]
    fn list_and_cons() {
        assert_eq!(eval(r"[1, 2, 3]"), "[1, 2, 3]");
        assert_eq!(eval(r"1 <> [2, 3]"), "[1, 2, 3]");
    }

    #[test]
    fn match_not() {
        let src = r"not = ?{ T => F; F => T }; not T";
        assert_eq!(eval(src), "#F");
    }

    #[test]
    fn match_with_fields() {
        // a constructor branch is applied to the constructor's fields
        let src = r"fst = ?{ Pair => \a b -> a }; fst Pair{1, 2}";
        assert_eq!(eval(src), "1");
    }

    #[test]
    fn match_list() {
        // length-ish: head of a list
        let src = r"head = ?{ <> => \h t -> h; [] => 0 }; head [7, 8, 9]";
        assert_eq!(eval(src), "7");
    }

    #[test]
    fn numeric_switch() {
        let src = r"f = ?{ 0 => 100; 1 => 200 }; f 1";
        assert_eq!(eval(src), "200");
    }

    #[test]
    fn superposition_output() {
        // normalize keeps superpositions in the output
        assert_eq!(eval(r"&L{1, 2}"), "&L{1, 2}");
    }

    #[test]
    fn op_over_sup() {
        // an operation distributes over a superposition
        assert_eq!(eval(r"&L{1, 2} + 10"), "&L{11, 12}");
    }

    #[test]
    fn strings_and_chars() {
        assert_eq!(eval(r"'A'"), "#Chr{65}");
        // "hi" is a list of #Chr
        assert_eq!(eval(r#""hi""#), "[#Chr{104}, #Chr{105}]");
    }

    #[test]
    fn erasure() {
        // applying an erased value yields erasure
        assert_eq!(eval(r"(\_ -> _) 5"), "_");
    }

    #[test]
    fn erasing_lambda_discards_arg() {
        // an unused binder becomes an erasing lambda; the (large) argument is
        // erased and the body returned
        assert_eq!(eval(r"(\_ -> 7) [1, 2, 3]"), "7");
        // K discards its second argument
        assert_eq!(eval(r"(\x y -> x) 1 [9, 9, 9]"), "1");
    }

    #[test]
    fn div_by_zero_is_erasure() {
        assert_eq!(eval(r"10 / 0"), "*");
        assert_eq!(eval(r"7 % 0"), "*");
        // a normal division still works
        assert_eq!(eval(r"10 / 2"), "5");
    }

    #[test]
    fn era_bubbles_through_eliminators() {
        // applying an erasure erases the argument and yields an erasure
        assert_eq!(eval(r"(10 / 0) 5"), "*");
        // an erased operand annihilates an operation
        assert_eq!(eval(r"(10 / 0) + 99"), "*");
        assert_eq!(eval(r"99 + (10 / 0)"), "*");
    }

    #[test]
    fn explicit_dup_requires_both_sides() {
        // both projections of an explicit dup must be used
        assert!(run(r"&L{a b} = 5; a").is_err());
        assert!(run(r"(\&L{a b} -> a) 5").is_err());
        // using both is fine
        assert_eq!(eval(r"&L{a b} = 5; [a, b]"), "[5, 5]");
    }

    // --- async primitives (engine-only) ---

    /// `%slow n`: an async primitive that yields `n` after one runtime yield.
    /// `inflight`/`max_inflight` are bumped while its future is pending so a
    /// test can prove two `%slow` calls were awaited *concurrently*.
    #[derive(Clone)]
    struct AsyncExt {
        inflight: Arc<AtomicUsize>,
        max_inflight: Arc<AtomicUsize>,
    }

    const SLOW: PrimId = PrimId(0);

    impl Extensions for AsyncExt {
        fn resolve(&self, name: &str) -> Option<PrimId> {
            (name == "slow").then_some(SLOW)
        }
        fn arity(&self, _: PrimId) -> usize {
            1
        }
        fn name(&self, _: PrimId) -> Option<Cow<'_, str>> {
            Some("slow".into())
        }
        fn apply(&self, heap: &mut Heap, _id: PrimId, args: &[Node]) -> PrimResult {
            let Term::Num(n) = args[0].unpack() else {
                return PrimResult::Done(heap.era());
            };
            let inflight = self.inflight.clone();
            let max = self.max_inflight.clone();
            let fut: PrimFuture = Box::pin(async move {
                // mark in-flight, record the peak concurrency, then yield once so
                // the future is observed Pending (and a sibling can also start).
                let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                max.fetch_max(now, Ordering::SeqCst);
                tokio::task::yield_now().await;
                inflight.fetch_sub(1, Ordering::SeqCst);
                Term::Num(n).pack()
            });
            PrimResult::Pending(fut)
        }
    }

    fn async_ext() -> (AsyncExt, Arc<AtomicUsize>) {
        let max = Arc::new(AtomicUsize::new(0));
        let ext = AsyncExt {
            inflight: Arc::new(AtomicUsize::new(0)),
            max_inflight: max.clone(),
        };
        (ext, max)
    }

    #[test]
    fn async_primitive_resolves() {
        let (ext, _max) = async_ext();
        assert_eq!(run_with("%slow 7", ext).unwrap(), "7");
    }

    #[test]
    fn async_primitive_inside_expression() {
        // the async result flows into a normal interaction
        let (ext, _max) = async_ext();
        assert_eq!(run_with("(%slow 7) + 1", ext).unwrap(), "8");
    }

    #[test]
    fn both_operands_awaited_concurrently() {
        // `(%slow 10 + %slow 20)` must drive BOTH async branches before yielding,
        // so peak in-flight reaches 2 (they are awaited at the same time).
        let (ext, max) = async_ext();
        assert_eq!(run_with("%slow 10 + %slow 20", ext).unwrap(), "30");
        assert_eq!(
            max.load(Ordering::SeqCst),
            2,
            "both async primitives should have been in flight simultaneously"
        );
    }
}
