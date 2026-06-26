pub mod exec;
pub mod heap;
pub mod printer;
pub mod term;

use crate::core::ast::desugar;
use crate::core::parse::parse;
use crate::extension::{Extensions, NoExtensions};
use exec::{Executor, UnlimitedBudget};
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
    use super::exec::{ExecPolicy, Executor};
    use super::{run, run_with};
    use crate::extension::{Extensions, Handle, PrimReduce};
    use crate::vm::term::{PrimId, Term};
    use std::borrow::Cow;

    /// A tiny arithmetic extension: `%add`/`%mul` (sync, arity 2) and `%inc`
    /// (async, arity 1).
    struct Arith;

    /// Force an argument handle to WHNF and read its `u64`. The handle is simply
    /// dropped afterwards; the executor reclaims its node via
    /// `erase_dropped_handles` — the primitive never erases by hand.
    async fn force_u64<'e, 'h, P, X>(exec: &Executor<'e, 'h, P, X>, h: Handle<'h>) -> u64
    where
        P: ExecPolicy,
        X: Extensions,
    {
        let h = exec.whnf_at(h).await;
        match &*h.view() {
            Term::U64(x) => *x,
            _ => 0,
        }
    }

    impl Extensions for Arith {
        fn resolve(&self, name: &str) -> Option<PrimId> {
            match name {
                "add" => Some(PrimId::new(0)),
                "mul" => Some(PrimId::new(1)),
                "inc" => Some(PrimId::new(2)),
                "fst" => Some(PrimId::new(3)),
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
                3 => "fst",
                _ => "?",
            }))
        }
        fn apply<'a, 'e, 'h, P: ExecPolicy>(
            &'a self,
            exec: &'a Executor<'e, 'h, P, Self>,
            id: PrimId,
            args: Vec<Handle<'h>>,
        ) -> PrimReduce<'a, 'h> {
            Box::pin(async move {
                let mut it = args.into_iter();
                let result = match id.get() {
                    0 => {
                        let a = force_u64(exec, it.next().unwrap()).await;
                        let b = force_u64(exec, it.next().unwrap()).await;
                        exec.heap.alloc(Term::U64(a + b))
                    }
                    1 => {
                        let a = force_u64(exec, it.next().unwrap()).await;
                        let b = force_u64(exec, it.next().unwrap()).await;
                        exec.heap.alloc(Term::U64(a * b))
                    }
                    2 => {
                        let a = force_u64(exec, it.next().unwrap()).await;
                        exec.heap.alloc(Term::U64(a + 1))
                    }
                    3 => {
                        // `fst`: force and keep the first argument; the second is
                        // never forced — its handle simply drops here and the
                        // executor reclaims the whole (unevaluated) subterm.
                        let a = force_u64(exec, it.next().unwrap()).await;
                        drop(it.next().unwrap());
                        exec.heap.alloc(Term::U64(a))
                    }
                    _ => unreachable!(),
                };
                Handle::new(result, exec.heap)
            })
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
    fn prim_ignores_unused_arg() {
        // `fst` keeps its first argument and never even forces the second; its
        // handle is dropped, and the executor reclaims the ignored subterm via
        // `erase_dropped_handles` — a constructor tree here, an unforced
        // primitive application below.
        assert_eq!(run_with(r"%fst 7 [1, 2, 3]", &Arith).unwrap(), "7");
        assert_eq!(run_with(r"%fst (%inc 41) (%mul 9 9)", &Arith).unwrap(), "42");
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
        assert_eq!(run(r"[1, 2]").unwrap(), "Con{1, Con{2, []}}");
    }

    #[test]
    fn bool_and_float_literals() {
        // `true`/`false` are scalar bool values (not nullary constructors); float
        // literals are parsed to f64. A trailing-`.0` float prints without it.
        assert_eq!(run(r"true").unwrap(), "true");
        assert_eq!(run(r"false").unwrap(), "false");
        assert_eq!(run(r"3.14").unwrap(), "3.14");
        assert_eq!(run(r"[true, 3.5]").unwrap(), "Con{true, Con{3.5, []}}");
        // a capitalized word is still a constructor, not a bool
        assert_eq!(run(r"True").unwrap(), "True");
        // `truex` is an ordinary identifier, not the `true` keyword
        assert!(run(r"truex").is_err());
    }

    #[test]
    fn string_and_char_values() {
        // Strings are boxed scalar values, not desugared character lists; chars
        // are scalar `Char`s, not `Chr` constructors.
        assert_eq!(run(r#""hello""#).unwrap(), r#""hello""#);
        assert_eq!(run(r"'a'").unwrap(), "'a'");
        // A string inside a list stays a single value element.
        assert_eq!(run(r#"["hi", 'x']"#).unwrap(), r#"Con{"hi", Con{'x', []}}"#);
    }

    #[test]
    fn normalizes_under_lambda() {
        // x is used once -> a real (non-erasing) lambda; the body is normalized.
        assert_eq!(run(r"\x -> x + 1").unwrap(), r"\a -> (a + 1)");
    }
}
