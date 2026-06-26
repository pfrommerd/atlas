pub mod exec;
pub mod heap;
pub mod printer;
pub mod term;

use crate::core::ast::desugar;
use crate::core::parse::parse;
use exec::{Executor, Extensions, NoExtensions, UnlimitedBudget};
use heap::Heap;
use printer::Printer;

/// Parse, desugar, lower, normalize, and pretty-print a source expression.
pub fn run(src: &str) -> Result<String, String> {
    run_with(src, &NoExtensions)
}

/// Like [`run`], but with a host-provided primitive [`Extensions`] set.
pub fn run_with<X: Extensions>(src: &str, ext: &X) -> Result<String, String> {
    let node = parse(src)?;
    let expr = desugar(&node)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| e.to_string())?;
    let heap = Heap::new();
    heap.with(|h| {
        let resolve = |n: &str| ext.resolve(n);
        let root = h.lower(&expr, &resolve)?;
        let exec = Executor::with_extensions(h, UnlimitedBudget, ext);
        let result = rt.block_on(exec.normalize_at(root));
        Ok(format!("{}", Printer::new(h).pretty(&result)))
    })
}

#[cfg(test)]
mod tests {
    use super::exec::{Extensions, PrimResult};
    use super::{run, run_with};
    use crate::vm::term::{Node, PrimId, Term};
    use std::borrow::Cow;

    /// A tiny arithmetic extension: `%add`/`%mul` (sync, arity 2) and `%inc`
    /// (async, arity 1).
    struct Arith;

    impl Arith {
        fn arg(n: &Node) -> u64 {
            match unsafe { n.unpack() } {
                Term::U64(x) => x,
                _ => 0,
            }
        }
    }

    impl Extensions for Arith {
        fn resolve(&self, name: &str) -> Option<PrimId> {
            match name {
                "add" => Some(PrimId::new(0)),
                "mul" => Some(PrimId::new(1)),
                "inc" => Some(PrimId::new(2)),
                _ => None,
            }
        }
        fn arity(&self, id: PrimId) -> usize {
            if id.get() == 2 { 1 } else { 2 }
        }
        fn name(&self, id: PrimId) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(match id.get() {
                0 => "add",
                1 => "mul",
                2 => "inc",
                _ => "?",
            }))
        }
        fn apply<'h>(
            &self,
            _heap: &super::heap::HeapScope<'h>,
            id: PrimId,
            args: &[Node],
        ) -> PrimResult {
            match id.get() {
                0 => PrimResult::Done(Term::U64(Self::arg(&args[0]) + Self::arg(&args[1])).pack()),
                1 => PrimResult::Done(Term::U64(Self::arg(&args[0]) * Self::arg(&args[1])).pack()),
                2 => {
                    let n = Self::arg(&args[0]);
                    PrimResult::Pending(Box::pin(async move { Term::U64(n + 1).pack() }))
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn prim_sync() {
        assert_eq!(run_with(r"%add 2 3", &Arith).unwrap(), "5");
        assert_eq!(run_with(r"%mul 4 5", &Arith).unwrap(), "20");
        assert_eq!(run_with(r"%add (%mul 2 3) 4", &Arith).unwrap(), "10");
    }

    #[test]
    fn prim_async() {
        assert_eq!(run_with(r"%inc 41", &Arith).unwrap(), "42");
    }

    #[test]
    fn auto_dup() {
        // `\&x -> x + x` duplicates its argument (the dup value is the binder,
        // read lazily after substitution).
        assert_eq!(run(r"(\&x -> x + x) 5").unwrap(), "10");
        assert_eq!(run(r"(\&x -> x * x) 4").unwrap(), "16");
        // Used N times -> a desugared chain of N-1 dups (handled entirely in
        // lowering; the executor has no auto-dup rule).
        assert_eq!(run(r"(\&x -> x + x + x) 2").unwrap(), "6");
        assert_eq!(run(r"(\&x -> x + x + x + x) 3").unwrap(), "12");
        assert_eq!(run(r"(\&x -> x * x * x * x) 2").unwrap(), "16");
    }

    #[test]
    fn dup_readback_hoists_bindings() {
        // A stuck dup (its value is the binder `c`) reads back as a single
        // `&{l, r} = value` binding ahead of the body, naming both projections.
        assert_eq!(run(r"\&x -> x + x").unwrap(), "&{a, b} = c;\n\\c -> (a + b)");
        // Chained dups are emitted in dependency order: the second binding's value
        // is `b` (the first dup's Dp1), so it follows the first.
        assert_eq!(
            run(r"\&x -> x + x + x").unwrap(),
            "&{a, b} = c;\n&{d, e} = b;\n\\c -> ((a + d) + e)"
        );
    }

    #[test]
    fn cloned_let_duplicates_lambda() {
        // `&x = \y -> ...; (x 1) + (x 2)` shares one lambda and duplicates it via
        // a dup chain. When the lambda body holds a stuck binary op (an operand is
        // the binder), DUP-BOP distributes the dup into both operands.
        assert_eq!(run(r"&x = \y -> 2 * y; (x 1) + (x 2)").unwrap(), "6");
        assert_eq!(run(r"&x = \y -> (2 + 1) * y; (x 1) + (x 2)").unwrap(), "9");
        assert_eq!(
            run(r"&x = \y -> (2 + 1) * y; (x 1) + (x 2) + (x 3)").unwrap(),
            "18"
        );
    }

    #[test]
    fn bop_sup() {
        // A superposed operand distributes the op over both branches.
        assert_eq!(run(r"&L{1, 2} + 10").unwrap(), "&{11, 12}");
        assert_eq!(run(r"100 - &L{1, 2}").unwrap(), "&{99, 98}");
    }

    #[test]
    fn superposition_application() {
        // Applying a superposition of two functions duplicates the argument
        // (DUP-NUM) and applies each side.
        assert_eq!(run(r"(&L{\x -> x, \y -> y}) 5").unwrap(), "&{5, 5}");
    }

    #[test]
    fn match_numbers() {
        assert_eq!(run(r"?{1 => 100; 2 => 200} 1").unwrap(), "100");
        assert_eq!(run(r"?{1 => 100; 2 => 200} 2").unwrap(), "200");
    }

    #[test]
    fn match_constructors() {
        assert_eq!(run(r"?{[] => 7; Con => 0} []").unwrap(), "7");
        // Con branch is applied to the head and tail fields.
        assert_eq!(run(r"?{Con => \h t -> h; [] => 0} [9]").unwrap(), "9");
    }

    #[test]
    fn identity() {
        assert_eq!(run(r"(\x -> x) 42").unwrap(), "42");
    }

    #[test]
    fn k_combinator_erases_unused() {
        // \x y -> x : applying to 1 and 2 returns 1 and erases 2.
        assert_eq!(run(r"(\x y -> x) 1 2").unwrap(), "1");
    }

    #[test]
    fn arithmetic() {
        assert_eq!(run(r"2 + 3").unwrap(), "5");
        assert_eq!(run(r"(\x -> x + 1) 10").unwrap(), "11");
    }

    #[test]
    fn constructor_data() {
        assert_eq!(run(r"[1, 2]").unwrap(), "#Con{1, #Con{2, []}}");
    }

    #[test]
    fn normalizes_under_lambda() {
        // x is used once -> a real (non-erasing) lambda; the body is normalized.
        assert_eq!(run(r"\x -> x + 1").unwrap(), r"\a -> (a + 1)");
    }
}
