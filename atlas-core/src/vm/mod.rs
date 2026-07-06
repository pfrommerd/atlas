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
        // A closed program (run via `run_with`) has no free REPL locals.
        let root = h.lower(&expr, &resolve, &mut |_| None)?;
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
    async fn force_int<'e, 'h, P, X>(exec: &Executor<'e, 'h, P, X>, h: Handle<'h>) -> i64
    where
        P: ExecPolicy,
        X: Extensions,
    {
        let h = exec.whnf_at(h).await;
        match &*h.view() {
            Term::Int(x) => *x,
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
                        let a = force_int(exec, it.next().unwrap()).await;
                        let b = force_int(exec, it.next().unwrap()).await;
                        exec.heap.alloc(Term::Int(a + b))
                    }
                    1 => {
                        let a = force_int(exec, it.next().unwrap()).await;
                        let b = force_int(exec, it.next().unwrap()).await;
                        exec.heap.alloc(Term::Int(a * b))
                    }
                    2 => {
                        let a = force_int(exec, it.next().unwrap()).await;
                        exec.heap.alloc(Term::Int(a + 1))
                    }
                    3 => {
                        // `fst`: force and keep the first argument; the second is
                        // never forced — its handle simply drops here and the
                        // executor reclaims the whole (unevaluated) subterm.
                        let a = force_int(exec, it.next().unwrap()).await;
                        drop(it.next().unwrap());
                        exec.heap.alloc(Term::Int(a))
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
    fn partial_primitive_completes_when_applied() {
        // `%add 2` is a partial primitive (a `Partial`); applying its second
        // argument completes and fires it.
        assert_eq!(run_with(r"(\&f -> f 3) (%add 2)", &Arith).unwrap(), "5");
    }

    #[test]
    fn prim_ignores_unused_arg() {
        // `fst` keeps its first argument and never even forces the second; its
        // handle is dropped, and the executor reclaims the ignored subterm via
        // `erase_dropped_handles` (an unforced primitive application here).
        assert_eq!(run_with(r"%fst 7 (%add 1 2)", &Arith).unwrap(), "7");
        assert_eq!(
            run_with(r"%fst (%inc 41) (%mul 9 9)", &Arith).unwrap(),
            "42"
        );
    }

    #[test]
    fn auto_dup() {
        // `\&x -> x + x` duplicates its argument (the dup value is the binder,
        // read lazily after substitution).
        assert_eq!(run(r"(\&x -> x + x) 5").unwrap(), "10");
        assert_eq!(run(r"(\&x -> x * x) 4").unwrap(), "16");
        // Used N times -> an N-1 chain of binary dups.
        assert_eq!(run(r"(\&x -> x + x + x) 2").unwrap(), "6");
        assert_eq!(run(r"(\&x -> x + x + x + x) 3").unwrap(), "12");
        assert_eq!(run(r"(\&x -> x * x * x * x) 2").unwrap(), "16");
    }

    #[test]
    fn explicit_n_way_dup() {
        // `&{a b c}` expands to a binary dup chain over the value.
        assert_eq!(run(r"&{a b c} = 5; (a + b) + c").unwrap(), "15");
        assert_eq!(
            run(r"&{a b c d e} = 2; ((((a + b) + c) + d) + e)").unwrap(),
            "10"
        );
    }

    #[test]
    fn dup_over_lambda_n_ways() {
        // A cloned lambda used three times expands to a binary dup chain, with
        // each copy applied to its own argument.
        assert_eq!(
            run(r"&f = \y -> y + 1; ((f 1) + (f 2)) + (f 3)").unwrap(),
            "9"
        );
    }

    #[test]
    fn nested_clones_combine() {
        // `&y = x` duplicates a projection of the `&x` dup; the binary chain
        // still preserves sharing so all uses see the same value.
        assert_eq!(run(r"&x = 10; &y = x; (y + y) + x").unwrap(), "30");
        assert_eq!(
            run(r"&x = \z -> z * 2; &g = x; ((g 1) + (g 2)) + (x 3)").unwrap(),
            "12"
        );
    }

    #[test]
    fn dup_readback_hoists_bindings() {
        // A stuck dup (its value is the binder `c`) reads back as a single
        // `&{l, r} = value` binding ahead of the body, naming both projections.
        assert_eq!(
            run(r"\&x -> x + x").unwrap(),
            "&{a, b} = c;\n\\c -> (a + b)"
        );
        // Three uses are a binary dup chain.
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
        assert_eq!(run(r"&{1, 2} + 10").unwrap(), "&{11, 12}");
        assert_eq!(run(r"100 - &{1, 2}").unwrap(), "&{99, 98}");
    }

    #[test]
    fn superposition_application() {
        // Applying a superposition of two functions duplicates the argument
        // (DUP-NUM) and applies each side.
        assert_eq!(run(r"(&{\x -> x, \y -> y}) 5").unwrap(), "&{5, 5}");
    }

    #[test]
    fn match_numbers() {
        assert_eq!(run(r"?{1 -> 100; 2 -> 200} 1").unwrap(), "100");
        assert_eq!(run(r"?{1 -> 100; 2 -> 200} 2").unwrap(), "200");
    }

    #[test]
    fn dup_over_match_table() {
        assert_eq!(
            run(r"&m = ?{0 -> 10; 1 -> 20; _ -> 30}; (m 0) + (m 1)").unwrap(),
            "30"
        );
    }

    #[test]
    fn match_all_primitive_literals() {
        // patterns now key on full primitive values, not just int/bool.
        assert_eq!(run(r#"?{"hi" -> 1; "bye" -> 2} "bye""#).unwrap(), "2");
        assert_eq!(run(r"?{'a' -> 1; 'b' -> 2} 'b'").unwrap(), "2");
        assert_eq!(run(r"?{3.5 -> 1; 4.5 -> 2} 4.5").unwrap(), "2");
        // `0` (Int) and `false` (Bool) no longer collide: in one table the int
        // scrutinee picks the int arm and the bool scrutinee picks the bool arm.
        assert_eq!(run(r"?{0 -> 1; false -> 2} 0").unwrap(), "1");
        assert_eq!(run(r"?{0 -> 1; false -> 2} false").unwrap(), "2");
    }

    #[test]
    fn division_semantics() {
        // `/` is true division: always a float, even for two ints.
        assert_eq!(run(r"7 / 2").unwrap(), "3.5");
        assert_eq!(run(r"6 / 2").unwrap(), "3.0");
        assert_eq!(run(r"7.0 / 2.0").unwrap(), "3.5");
        // division by zero is an Err, not `inf`.
        assert_eq!(run(r"7 / 0").unwrap(), "<err>");
        assert_eq!(run(r"7.0 / 0.0").unwrap(), "<err>");
        // `~/` is floor (integer) division.
        assert_eq!(run(r"7 ~/ 2").unwrap(), "3");
        assert_eq!(run(r"-7 ~/ 2").unwrap(), "-4");
        assert_eq!(run(r"7 ~/ 0").unwrap(), "<err>");
        // `~/` with a float operand floor-divides and keeps a float.
        assert_eq!(run(r"7.0 ~/ 2").unwrap(), "3.0");
        // `//` remains a line comment, so the trailing text is ignored.
        assert_eq!(run("5 // not division").unwrap(), "5");
    }

    #[test]
    fn uncovered_match_is_err() {
        // A concrete value with no covering case and no default is a runtime err.
        assert_eq!(run(r"?{1 -> 100; 2 -> 200} 3").unwrap(), "<err>");
        assert_eq!(run(r"?{true -> 1} false").unwrap(), "<err>");
        // A default still covers the otherwise-uncovered scrutinee.
        assert_eq!(run(r"?{1 -> 100; _ -> 0} 3").unwrap(), "0");
    }

    #[test]
    fn binding_default_receives_scrutinee() {
        // An `x ->` default binds the whole scrutinee that failed every case.
        assert_eq!(run(r"?{1 -> 0; x -> x + 1} 3").unwrap(), "4");
        // A `_ ->` default erases the scrutinee.
        assert_eq!(run(r"?{1 -> 0; _ -> 1} 3").unwrap(), "1");
        // A matching case still wins over the default.
        assert_eq!(run(r"?{1 -> 0; x -> x + 1} 1").unwrap(), "0");
        // A constructor scrutinee reaches the default as the whole value, not its
        // unboxed fields: rebind and re-match to recover the payload.
        let opt = r"Opt = \ T -> type { Some(T), None }; ";
        assert_eq!(
            run(&format!(
                "{opt}?{{None -> 0; x -> ?{{Some y -> y; None -> 0}} x}} ((Opt (type ()))::Some 5)"
            ))
            .unwrap(),
            "5"
        );
    }

    #[test]
    fn auto_dup_default_receives_scrutinee() {
        // An `&x ->` default binds the whole scrutinee auto-dup: it may be used
        // any number of times in the body (a dup chain, as with a `\&x` binder).
        assert_eq!(run(r"?{&x -> x + x} 1").unwrap(), "2");
        assert_eq!(run(r"?{1 -> 0; &x -> x + x} 3").unwrap(), "6");
        assert_eq!(run(r"?{1 -> 0; &x -> x + x + x} 2").unwrap(), "6");
        // A matching case still wins over the auto-dup default.
        assert_eq!(run(r"?{1 -> 0; &x -> x + x} 1").unwrap(), "0");
        // Degenerate arities collapse like `\&x`: zero uses erases, one is plain.
        assert_eq!(run(r"?{1 -> 0; &x -> 9} 3").unwrap(), "9");
        assert_eq!(run(r"?{1 -> 0; &x -> x + 1} 3").unwrap(), "4");
        // Only one default arm is allowed, whatever its form.
        assert!(run(r"?{x -> 1; &y -> 2} 3").is_err());
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
    fn unary_ops() {
        // negation: integer literals are i64, so `-2` is just -2.
        assert_eq!(run(r"-2").unwrap(), "-2");
        // float negation flips the sign.
        assert_eq!(run(r"-2.5").unwrap(), "-2.5");
        // logical not on bool.
        assert_eq!(run(r"~true").unwrap(), "false");
        assert_eq!(run(r"~false").unwrap(), "true");
        // bitwise complement on an integer.
        assert_eq!(run(r"~5").unwrap(), (!5i64).to_string());
        // prefix binds tighter than infix: `-2 + 3` == `(-2) + 3`.
        assert_eq!(run(r"-2 + 3").unwrap(), "1");
        // unary not is unsupported on a char -> Err.
        assert_eq!(run(r"~'a'").unwrap(), "<err>");
    }

    #[test]
    fn binary_ops_on_primitives() {
        // float arithmetic and comparison (float results always print a `.`).
        assert_eq!(run(r"1.5 + 2.5").unwrap(), "4.0");
        assert_eq!(run(r"3.0 < 5.0").unwrap(), "true");
        // integer comparisons now yield Bool.
        assert_eq!(run(r"3 < 5").unwrap(), "true");
        assert_eq!(run(r"5 == 5").unwrap(), "true");
        assert_eq!(run(r"5 != 5").unwrap(), "false");
        // mixed int/float arithmetic promotes the int and yields a float.
        assert_eq!(run(r"1 + 2.5").unwrap(), "3.5");
        assert_eq!(run(r"2.5 + 1").unwrap(), "3.5");
        assert_eq!(run(r"2 < 3.0").unwrap(), "true");
        // `&&`/`||` are eager bitwise ops on integers (surface has no bare `&`/`|`).
        assert_eq!(run(r"6 && 3").unwrap(), (6i64 & 3).to_string());
        assert_eq!(run(r"6 || 1").unwrap(), (6i64 | 1).to_string());
        // char comparison.
        assert_eq!(run(r"'a' == 'a'").unwrap(), "true");
        assert_eq!(run(r"'a' < 'b'").unwrap(), "true");
        // bool logical ops.
        assert_eq!(run(r"true && false").unwrap(), "false");
        assert_eq!(run(r"true || false").unwrap(), "true");
    }

    #[test]
    fn invalid_ops_become_err() {
        // division / modulo / integer-division by zero.
        assert_eq!(run(r"1 / 0").unwrap(), "<err>");
        assert_eq!(run(r"1 % 0").unwrap(), "<err>");
        assert_eq!(run(r"1 ~/ 0").unwrap(), "<err>");
        // type mismatch between concrete values (numeric vs non-numeric).
        assert_eq!(run(r"1 + 'a'").unwrap(), "<err>");
        // op unsupported for the operand type.
        assert_eq!(run(r"true + true").unwrap(), "<err>");
        // bitwise ops are invalid on floats.
        assert_eq!(run(r"1.0 && 2.0").unwrap(), "<err>");
    }

    #[test]
    fn string_binary_ops() {
        // equality / inequality on strings.
        assert_eq!(run(r#""ab" == "ab""#).unwrap(), "true");
        assert_eq!(run(r#""ab" == "ac""#).unwrap(), "false");
        assert_eq!(run(r#""ab" != "ac""#).unwrap(), "true");
        // concatenation produces a fresh string.
        assert_eq!(run(r#""foo" + "bar""#).unwrap(), r#""foobar""#);
        assert_eq!(run(r#""" + "x""#).unwrap(), r#""x""#);
        // an unsupported op on strings is an error.
        assert_eq!(run(r#""a" - "b""#).unwrap(), "<err>");
        // a string compared against a scalar is a type mismatch -> error.
        assert_eq!(run(r#""a" == 1"#).unwrap(), "<err>");
    }

    #[test]
    fn err_bubbles_up_when_forced() {
        // div-by-zero produces an Err that propagates through enclosing ops
        // instead of getting stuck.
        assert_eq!(run(r"(1 / 0) + 5").unwrap(), "<err>");
        assert_eq!(run(r"5 + (1 / 0)").unwrap(), "<err>");
        assert_eq!(run(r"-(1 / 0)").unwrap(), "<err>");
        assert_eq!(run(r"~(1 / 0)").unwrap(), "<err>");
        // applied as a function, an Err erases its argument and bubbles up.
        assert_eq!(run(r"(1 / 0) 5").unwrap(), "<err>");
        assert_eq!(run(r"(1 / 0) 5 6 7").unwrap(), "<err>");
        // nested: the Err threads all the way out.
        assert_eq!(run(r"((1 / 0) 5) + 2").unwrap(), "<err>");
    }

    #[test]
    fn match_on_bool() {
        // a comparison result (Bool) is matchable via true/false literal patterns.
        assert_eq!(run(r"?{true -> 1; false -> 0} (3 < 5)").unwrap(), "1");
        assert_eq!(run(r"?{true -> 1; false -> 0} (5 < 3)").unwrap(), "0");
    }

    #[test]
    fn bool_and_float_literals() {
        // `true`/`false` are scalar bool values (not nullary constructors); float
        // literals are parsed to f64 and always print with a decimal point.
        assert_eq!(run(r"true").unwrap(), "true");
        assert_eq!(run(r"false").unwrap(), "false");
        assert_eq!(run(r"3.14").unwrap(), "3.14");
        assert_eq!(run(r"3.0").unwrap(), "3.0");
        // a capitalized word is now an ordinary variable (here unbound), not a bool
        assert!(run(r"True").is_err());
        // `truex` is an ordinary identifier, not the `true` keyword
        assert!(run(r"truex").is_err());
    }

    #[test]
    fn string_and_char_values() {
        // Strings are boxed scalar values, not desugared character lists; chars
        // are scalar `Char`s, not `Chr` constructors.
        assert_eq!(run(r#""hello""#).unwrap(), r#""hello""#);
        assert_eq!(run(r"'a'").unwrap(), "'a'");
    }

    #[test]
    fn normalizes_under_lambda() {
        // x is used once -> a real (non-erasing) lambda; the body is normalized.
        assert_eq!(run(r"\x -> x + 1").unwrap(), r"\a -> (a + 1)");
    }
}

#[cfg(test)]
mod type_system_tests {
    use super::exec::{ExecPolicy, Executor, InteractionType};
    use super::run;
    use crate::vm::term::Term;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct CountingPolicy {
        counts: Mutex<HashMap<InteractionType, u64>>,
    }

    impl CountingPolicy {
        fn count(&self, interaction: InteractionType) -> u64 {
            *self.counts.lock().unwrap().get(&interaction).unwrap_or(&0)
        }
    }

    impl ExecPolicy for CountingPolicy {
        fn next_step(&self, interaction: InteractionType) {
            *self.counts.lock().unwrap().entry(interaction).or_default() += 1;
        }

        fn should_continue(&self) -> bool {
            true
        }
    }

    #[test]
    fn typeof_scalars() {
        assert_eq!(run(r"typeof 5").unwrap(), "Int");
        assert_eq!(run(r"typeof 3.5").unwrap(), "Float");
        assert_eq!(run(r"typeof true").unwrap(), "Bool");
        assert_eq!(run(r"typeof 'a'").unwrap(), "Char");
        assert_eq!(run(r#"typeof "hi""#).unwrap(), "String");
    }

    // NOTE: builtin type names (`Int`, `Bool`, …) are now resolved by a (not-yet-
    // implemented) prelude, so the tests below use an inline empty product `type ()`
    // as a stand-in type argument. Field/arg types are lazy and never forced, so the
    // exact type used does not affect construction.

    #[test]
    fn type_printing_shows_subexpressions() {
        // An anonymous type prints its (lazy, possibly unevaluated) sub-type
        // expressions rather than collapsing them.
        // Product fields keep their (unforced) expression form, e.g. the redex
        // `(\_ -> a) 1` is not reduced inside the type. (Readback renames the
        // bound variable to `a`.)
        assert_eq!(
            run(r"\T -> type ((\_ -> T) 1)").unwrap(),
            "\\a -> type(((\\_ -> a) 1))"
        );
        // Sum variants print their argument types too, rather than just names.
        assert_eq!(
            run(r"\T -> type { Some(T), None }").unwrap(),
            "\\a -> type{Some(a), None}"
        );
    }

    #[test]
    fn product_type_construction() {
        // A product is built through its `::New` constructor, which curries its
        // fields into a (variantless) construction.
        assert_eq!(
            run(r"Foo = type (type (), type ()); Foo::New 1 2").unwrap(),
            "<type>{1, 2}"
        );
        // Applying a bare type value (without `::New`) is an error.
        assert_eq!(
            run(r"Foo = type (type (), type ()); Foo 1 2").unwrap(),
            "<err>"
        );
    }

    #[test]
    fn new_is_reserved_as_variant_name() {
        // `New` names the product constructor, so it cannot be a sum variant.
        assert!(run(r"type { New }").is_err());
        assert!(run(r"type { New, Some(type ()) }").is_err());
    }

    #[test]
    fn empty_variant_type_is_rejected() {
        // A variant (sum) type must have at least one variant; `type {}` is an
        // error. (The empty *product* `type ()` is still the unit type.)
        assert!(run(r"type {}").is_err());
        assert!(run(r"\T -> type {}").is_err());
        assert!(run(r"type ()").is_ok());
    }

    #[test]
    fn sum_type_construction_and_typeof() {
        let opt = r"Opt = \ T -> type { Some(T), None }; ";
        // `::` selects a variant constructor of the type value; it curries.
        assert_eq!(
            run(&format!("{opt}(Opt (type ()))::Some 7")).unwrap(),
            "Some{7}"
        );
        // A nullary variant is a constructor with no fields.
        assert_eq!(run(&format!("{opt}(Opt (type ()))::None")).unwrap(), "None");
        // typeof a constructed value yields its (anonymous) declared type.
        assert_eq!(
            run(&format!("{opt}typeof ((Opt (type ()))::Some 7)")).unwrap(),
            "type{Some(type()), None}"
        );
    }

    #[test]
    fn match_over_user_variants() {
        let opt = r"Opt = \ T -> type { Some(T), None }; ";
        // Matching is by variant name (the scrutinee carries its variant id).
        assert_eq!(
            run(&format!(
                "{opt}?{{Some x -> x; None -> 0}} ((Opt (type ()))::Some 7)"
            ))
            .unwrap(),
            "7"
        );
        assert_eq!(
            run(&format!(
                "{opt}?{{Some x -> x; None -> 0}} (Opt (type ()))::None"
            ))
            .unwrap(),
            "0"
        );
    }

    #[test]
    fn constructors_saturate() {
        // A constructor accepts exactly its declared field count; further
        // arguments are left as a stuck application.
        assert_eq!(
            run(r"(type (type (), type ()))::New 1 2").unwrap(),
            "<type>{1, 2}"
        );
        assert_eq!(
            run(r"(type (type (), type ()))::New 1 2 3 4").unwrap(),
            "((<type>{1, 2} 3) 4)"
        );
        let opt = r"Opt = \ T -> type { Some(T), None }; ";
        assert_eq!(
            run(&format!("{opt}(Opt (type ()))::Some 7")).unwrap(),
            "Some{7}"
        );
        assert_eq!(
            run(&format!("{opt}(Opt (type ()))::Some 7 8 9")).unwrap(),
            "((Some{7} 8) 9)"
        );
        // A nullary variant saturates immediately.
        assert_eq!(
            run(&format!("{opt}(Opt (type ()))::None 5")).unwrap(),
            "(None 5)"
        );
    }

    #[test]
    fn partial_application_is_a_partial() {
        // An under-applied constructor is a `Partial`, printed as the callable
        // followed by its gathered args (not an under-filled `Ctn`). The product
        // constructor prints as `New`.
        assert_eq!(run(r"(type (type (), type ()))::New 1").unwrap(), "(New 1)");
        // A variant selector (awaiting args) is a value that prints as its name.
        let opt = r"Opt = \ T -> type { Some(T), None }; ";
        assert_eq!(run(&format!("{opt}(Opt (type ()))::Some")).unwrap(), "Some");
        // A multi-arg variant gathers args; partial then complete.
        let pair = r"Pair = \ A B -> type { Both(A, B) }; ";
        assert_eq!(
            run(&format!("{pair}(Pair (type ()) (type ()))::Both 1")).unwrap(),
            "(Both 1)"
        );
        assert_eq!(
            run(&format!("{pair}(Pair (type ()) (type ()))::Both 1 true")).unwrap(),
            "Both{1, true}"
        );
    }

    #[test]
    fn type_value_duplicates() {
        // A first-class type value can be duplicated and reused (`Type`/`Variant`/
        // `Partial` distribute over duplication). Here `ty` is used twice, so it is
        // duplicated; both uses build a `Some` of that type. (Lists aren't available
        // yet, so two uses are threaded through nested matches.)
        assert_eq!(
            run(
                r"(\&ty -> ?{Some x -> x; None -> 0} (ty::Some (?{Some y -> y; None -> 0} (ty::Some 5)))) (type { Some(type ()), None })"
            )
            .unwrap(),
            "5"
        );
    }

    #[test]
    fn duplicated_type_constructor_fn_substitutes() {
        // Duplicating a *type-returning* lambda and applying a copy must thread the
        // argument through the duplicated (lazy) sub-type fields, not drop it (the
        // `DUP-LAM`-over-`Type` path). A sum variant's arg and a product field both
        // resolve to the applied type, rather than a stray binder.
        assert_eq!(
            run(r"(\&f -> f (type ())) (\T -> type { Cons(T), Nil })").unwrap(),
            "type{Cons(type()), Nil}"
        );
        assert_eq!(
            run(r"(\&f -> f (type ())) (\T -> type (T))").unwrap(),
            "type(type())"
        );
    }

    #[test]
    fn duplicated_type_fn_reused_across_passes() {
        // The reported REPL bug: an auto-dup local bound to a type-returning lambda,
        // reused on *separate* evaluations. Each use must substitute its own argument
        // — which needs DUP-SUP annihilation to *wire* (defer the per-side pull) so
        // the not-yet-applied copy's binder is not snapshotted as a stray `Var`. A
        // single closed expression can't reproduce this (both binders fill within one
        // normalize pass), so this drives `dup_use` over a shared heap like the REPL.
        use super::exec::{Executor, UnlimitedBudget};
        use super::heap::Heap;
        use super::printer::Printer;
        use crate::core::ast::desugar;
        use crate::core::parse::parse;
        use crate::vm::term::Term;

        let lam = desugar(&parse(r"\T -> type { Cons(T), Nil }").unwrap()).unwrap();
        let unit = desugar(&parse(r"type ()").unwrap()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let heap = Heap::new();
        heap.with(|h| {
            let resolve = |_: &str| None;
            let mut local = |_: &str| None;
            // The auto-dup local's stored value; each use splices a fresh dup and
            // keeps the `Dp1` branch for the next use (mirrors `Locals::use_name`).
            let mut cur = h.lower(&lam, &resolve, &mut local).unwrap();
            let exec = Executor::new(h, UnlimitedBudget);
            for _ in 0..3 {
                let (use_node, keep_node) = h.dup_use(cur);
                cur = keep_node;
                let arg = h.lower(&unit, &resolve, &mut local).unwrap();
                let app = h.alloc(Term::App {
                    func: use_node,
                    arg,
                });
                let result = rt.block_on(exec.normalize_at(app));
                assert_eq!(
                    format!("{}", Printer::new(h).pretty(&result)),
                    "type{Cons(type()), Nil}"
                );
            }
        });
    }

    #[test]
    fn stacked_dup_chains_force_recursively() {
        use super::heap::Heap;
        use super::printer::Printer;

        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let heap = Heap::new();
        heap.with(|h| {
            let mut cur = h.alloc(Term::Int(1));
            let mut forced = None;
            for _ in 0..3 {
                let (use_node, keep_node) = h.dup_use(cur);
                forced = Some(use_node);
                cur = keep_node;
            }
            let exec = Executor::new(h, CountingPolicy::default());
            let result = rt.block_on(exec.normalize_at(forced.unwrap()));
            assert_eq!(format!("{}", Printer::new(h).pretty(&result)), "1");
        });
    }

    #[test]
    fn binary_dup_over_stuck_projection_stays_lazy() {
        // `g1` copies `\x -> y` (y an outer, unsubstituted binder), so applying
        // a `\&k` copy exposes a stuck projection over `y`. The binary dup stays
        // lazy and resolves correctly once `y` is substituted.
        use super::exec::{Executor, UnlimitedBudget};
        use super::heap::Heap;
        use super::printer::Printer;
        use crate::core::ast::desugar;
        use crate::core::parse::parse;
        use crate::vm::term::Term;

        let lam = desugar(
            &parse(r"\y -> (&{g1 g2} = \x -> y; ((\&k -> ((k 0) 1) + ((k 0) 2)) g1) + ((g2 0) 3))")
                .unwrap(),
        )
        .unwrap();
        let arg = desugar(&parse(r"\w -> w * 10").unwrap()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let heap = Heap::new();
        heap.with(|h| {
            let resolve = |_: &str| None;
            let mut local = |_: &str| None;
            let root = h.lower(&lam, &resolve, &mut local).unwrap();
            let exec = Executor::new(h, UnlimitedBudget);
            let stuck = rt.block_on(exec.normalize_at(root));
            // The `y` dup keeps its own binding; the k-dup reads back as a
            // separate binding chained off g1's projection (`a`).
            assert_eq!(
                format!("{}", Printer::new(h).pretty(&stuck)),
                "&{a, b} = c;\n&{d, e} = a;\n\\c -> (((d 1) + (e 2)) + (b 3))"
            );
            let argp = h.lower(&arg, &resolve, &mut local).unwrap();
            let app = h.alloc(Term::App {
                func: stuck,
                arg: argp,
            });
            let result = rt.block_on(exec.normalize_at(app));
            // (1*10) + (2*10) + (3*10)
            assert_eq!(format!("{}", Printer::new(h).pretty(&result)), "60");
        });
    }
}

/// Drop-tracking on dup cells: erased projections rewrite their surviving
/// parent nodes, and fired dups rewrite the non-forced parent directly.
#[cfg(test)]
mod dup_drop_tests {
    use super::exec::{ExecPolicy, Executor, InteractionType, UnlimitedBudget};
    use super::heap::{ArenaKind, Heap, MatchData};
    use super::printer::Printer;
    use crate::core::ast::desugar;
    use crate::core::parse::parse;
    use crate::vm::term::{BinaryOp, Term};
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct CountingPolicy {
        counts: Mutex<HashMap<InteractionType, u64>>,
    }

    impl CountingPolicy {
        fn count(&self, interaction: InteractionType) -> u64 {
            *self.counts.lock().unwrap().get(&interaction).unwrap_or(&0)
        }
    }

    impl ExecPolicy for CountingPolicy {
        fn next_step(&self, interaction: InteractionType) {
            *self.counts.lock().unwrap().entry(interaction).or_default() += 1;
        }
        fn should_continue(&self) -> bool {
            true
        }
    }

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    }

    #[test]
    fn drop_one_side_then_force_elides_copy() {
        // Erasing one projection before the other forces must hand the value
        // through uncopied (no DUP-LAM) and reclaim the cell.
        let lam = desugar(&parse(r"\x -> x + 1").unwrap()).unwrap();
        let rt = rt();
        let heap = Heap::new();
        heap.with(|h| {
            let lam = h.lower(&lam, &|_| None, &mut |_| None).unwrap();
            let (use_node, keep_node) = h.dup_use(lam);
            let exec = Executor::new(h, CountingPolicy::default());
            exec.erase(h.pull(keep_node));
            let app = h.alloc(Term::App {
                func: use_node,
                arg: h.alloc(Term::Int(1)),
            });
            let result = rt.block_on(exec.normalize_at(app));
            assert_eq!(format!("{}", Printer::new(h).pretty(&result)), "2");
            assert_eq!(exec.policy.count(InteractionType::DupErase), 0);
            assert_eq!(exec.policy.count(InteractionType::DupLam), 0);
            exec.erase(h.pull(result));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn drop_one_side_then_erase_survivor_reclaims_duplicand() {
        let heap = Heap::new();
        heap.with(|h| {
            let func = h.alloc(Term::Int(2));
            let arg = h.alloc(Term::Int(3));
            let (d0, d1) = h.alloc_dup(Term::App { func, arg });
            let label = h.dup_auto_label(d0);
            let n0 = h.alloc(Term::Dup { label, ptr: d0 });
            let n1 = h.alloc(Term::Dup { label, ptr: d1 });
            let exec = Executor::new(h, UnlimitedBudget);
            exec.erase(h.pull(n1));
            exec.erase(h.pull(n0));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn erase_after_fire_reclaims_loser_slot() {
        // Force one side (the winner fires and keeps its copy), then erase the
        // other side's projection instead of forcing it.
        let rt = rt();
        let heap = Heap::new();
        heap.with(|h| {
            let (d0, d1) = h.alloc_dup(Term::Int(5));
            let label = h.dup_auto_label(d0);
            let n0 = h.alloc(Term::Dup { label, ptr: d0 });
            let n1 = h.alloc(Term::Dup { label, ptr: d1 });
            let exec = Executor::new(h, CountingPolicy::default());
            let result = rt.block_on(exec.normalize_at(n0));
            assert_eq!(format!("{}", Printer::new(h).pretty(&result)), "5");
            assert_eq!(exec.policy.count(InteractionType::DupVal), 1);
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            exec.erase(h.pull(n1));
            exec.erase(h.pull(result));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn firing_dup_rewrites_other_parent() {
        // Firing one side rewrites the other projection's parent node directly;
        // there is no fired loser slot left in the dup cell.
        let rt = rt();
        let heap = Heap::new();
        heap.with(|h| {
            let (d0, d1) = h.alloc_dup(Term::Int(7));
            let label = h.dup_auto_label(d0);
            let exec = Executor::new(h, UnlimitedBudget);
            let n0 = h.alloc(Term::Dup { label, ptr: d0 });
            let n1 = h.alloc(Term::Dup { label, ptr: d1 });
            let r0 = rt.block_on(exec.normalize_at(n0));
            assert_eq!(format!("{}", Printer::new(h).pretty(&r0)), "7");
            assert_eq!(format!("{}", Printer::new(h).pretty(&n1)), "7");
            exec.erase(h.pull(r0));
            exec.erase(h.pull(n1));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn sup_crossing_erase_does_not_leak() {
        // Repeated use of an auto-dup lambda local: the crossing loop erases
        // rejected sup branches containing body-dup projections. With drops
        // recorded (and elision at fire time), everything reclaims.
        let lam = desugar(&parse(r"\x -> x + 1").unwrap()).unwrap();
        let one = desugar(&parse(r"1").unwrap()).unwrap();
        let rt = rt();
        let heap = Heap::new();
        heap.with(|h| {
            let resolve = |_: &str| None;
            let mut local = |_: &str| None;
            let mut cur = h.lower(&lam, &resolve, &mut local).unwrap();
            let exec = Executor::new(h, UnlimitedBudget);
            for _ in 0..3 {
                let (use_node, keep_node) = h.dup_use(cur);
                cur = keep_node;
                let arg = h.lower(&one, &resolve, &mut local).unwrap();
                let app = h.alloc(Term::App {
                    func: use_node,
                    arg,
                });
                let result = rt.block_on(exec.normalize_at(app));
                assert_eq!(format!("{}", Printer::new(h).pretty(&result)), "2");
                exec.erase(h.pull(result));
            }
            exec.erase(h.pull(cur));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Sups), 0);
            assert_eq!(h.arena_len(ArenaKind::Matches), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn stacked_dup_chain_reclaims_after_drops() {
        // Force the top of a three-deep dup chain, then erase the remaining
        // unforced projections: every cell and node must reclaim.
        let rt = rt();
        let heap = Heap::new();
        heap.with(|h| {
            let mut cur = h.alloc(Term::Int(1));
            let mut uses = Vec::new();
            for _ in 0..3 {
                let (use_node, keep_node) = h.dup_use(cur);
                uses.push(use_node);
                cur = keep_node;
            }
            let exec = Executor::new(h, CountingPolicy::default());
            let result = rt.block_on(exec.normalize_at(uses.pop().unwrap()));
            assert_eq!(format!("{}", Printer::new(h).pretty(&result)), "1");
            for u in uses {
                exec.erase(h.pull(u));
            }
            exec.erase(h.pull(cur));
            exec.erase(h.pull(result));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn readback_with_dropped_side() {
        // Dropping one projection rewrites the survivor's parent, so readback
        // sees the duplicand directly.
        let heap = Heap::new();
        heap.with(|h| {
            let (d0, d1) = h.alloc_dup(Term::Int(3));
            let label = h.dup_auto_label(d0);
            let n0 = h.alloc(Term::Dup { label, ptr: d0 });
            let n1 = h.alloc(Term::Dup { label, ptr: d1 });
            let exec = Executor::new(h, UnlimitedBudget);
            exec.erase(h.pull(n1));
            let printed = format!("{}", Printer::new(h).pretty(&n0));
            assert!(printed.contains('3'), "unexpected readback: {printed}");
            exec.erase(h.pull(n0));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn spine_bypass_hands_through_unreduced_value() {
        // A half-dropped dup met on the spine over a closed, sup/var-free
        // duplicand is bypassed without reducing under the cell lock: the
        // survivor takes the raw redex and the spine keeps reducing it.
        let rt = rt();
        let heap = Heap::new();
        heap.with(|h| {
            let lhs = h.alloc(Term::Int(1));
            let rhs = h.alloc(Term::Int(2));
            let (d0, d1) = h.alloc_dup(Term::Bop {
                op: BinaryOp::Add,
                lhs,
                rhs,
            });
            let label = h.dup_auto_label(d0);
            let exec = Executor::new(h, CountingPolicy::default());
            let n0 = h.alloc(Term::Dup { label, ptr: d0 });
            let n1 = h.alloc(Term::Dup { label, ptr: d1 });
            exec.erase(h.pull(n1));
            let result = rt.block_on(exec.normalize_at(n0));
            assert_eq!(format!("{}", Printer::new(h).pretty(&result)), "3");
            assert_eq!(exec.policy.count(InteractionType::DupErase), 0);
            exec.erase(h.pull(result));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn dropping_lambda_copy_drops_sup_side() {
        // DUP-LAM registers its manufactured shared-binder sup
        // `x ← &L{occ0, occ1}` with the body dup. Dropping one lambda copy
        // (one body-dup side) must eagerly drop the corresponding sup
        // component: it is erased on the spot, swapped for a `Wld` tombstone,
        // and the sup is collapse-marked to the survivor (the cell stays put —
        // both component slots are aliased binder addresses). The survivor's
        // reduction then unwraps the marked sup on contact and the body dup
        // elides its copy.
        let lam = desugar(&parse(r"\x -> x + 1").unwrap()).unwrap();
        let rt = rt();
        let heap = Heap::new();
        heap.with(|h| {
            let lam = h.lower(&lam, &|_| None, &mut |_| None).unwrap();
            let (use_node, keep_node) = h.dup_use(lam);
            let exec = Executor::new(h, CountingPolicy::default());
            // WHNF only: DUP-LAM fires (copying the lambda and manufacturing
            // the sup) but the body dups stay unforced, so the sup survives.
            let copy0 = rt.block_on(exec.whnf_at(use_node));
            assert_eq!(exec.policy.count(InteractionType::DupLam), 1);
            assert_eq!(h.arena_len(ArenaKind::Sups), 1);
            let copy1 = rt.block_on(exec.whnf_at(keep_node));
            // Dropping copy1 tombstones its sup component immediately (the
            // marked cell lingers until the survivor consumes it).
            let nodes_before = h.arena_len(ArenaKind::Nodes);
            exec.erase(h.pull(copy1));
            assert!(h.arena_len(ArenaKind::Nodes) < nodes_before);
            assert_eq!(h.arena_len(ArenaKind::Sups), 1);
            // The surviving copy applies correctly: the marked sup unwraps at
            // whnf (no DUP-SUP, no second lambda-body copy) and the body dup
            // elides.
            let app = h.alloc(Term::App {
                func: copy0,
                arg: h.alloc(Term::Int(1)),
            });
            let result = rt.block_on(exec.normalize_at(app));
            assert_eq!(format!("{}", Printer::new(h).pretty(&result)), "2");
            assert_eq!(exec.policy.count(InteractionType::DupSup), 0);
            assert_eq!(exec.policy.count(InteractionType::DupErase), 1);
            assert_eq!(exec.policy.count(InteractionType::DupLam), 1);
            exec.erase(h.pull(result));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Sups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn nested_sup_selected_after_copy_drop() {
        // The elision-hole regression: the duplicand's WHNF head is a
        // constructor, and the manufactured binder sup sits BELOW it (inside a
        // field). Eliding hands the whole structure through uncopied; the
        // nested sup must still resolve to the survivor's argument (via its
        // collapse mark), not read back as a stuck superposition.
        let lam = desugar(&parse(r"Pair = \A B -> type { Both(A, B) }; \x -> (Pair (type ()) (type ()))::Both x 99").unwrap()).unwrap();
        let rt = rt();
        let heap = Heap::new();
        heap.with(|h| {
            let lam = h.lower(&lam, &|_| None, &mut |_| None).unwrap();
            let (use_node, keep_node) = h.dup_use(lam);
            let exec = Executor::new(h, UnlimitedBudget);
            let copy0 = rt.block_on(exec.whnf_at(use_node));
            let copy1 = rt.block_on(exec.whnf_at(keep_node));
            exec.erase(h.pull(copy1));
            let app = h.alloc(Term::App {
                func: copy0,
                arg: h.alloc(Term::Int(7)),
            });
            let result = rt.block_on(exec.normalize_at(app));
            assert_eq!(
                format!("{}", Printer::new(h).pretty(&result)),
                "Both{7, 99}"
            );
            exec.erase(h.pull(result));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Sups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn commute_transfers_sup_registration() {
        // A different-label dup commuting over a registered sup replaces it
        // with TWO side-aligned sups; both must inherit the registration so a
        // later drop of the owning dup's side tombstones both replacements
        // (the "multiple sups per dup" case).
        let rt = rt();
        let heap = Heap::new();
        heap.with(|h| {
            // A sup S = &S{1, 2}, registered to owner dup cell O (whose own
            // duplicand is immaterial here — registration tracks feeding).
            let one = h.alloc(Term::Int(1));
            let two = h.alloc(Term::Int(2));
            let sup = h.sup(one, two);
            let sup_addr = sup.addr();
            let sup_label = LabelId::from_u56(h.intern_name("&S"));
            let sup_node = h.alloc(Term::Sup {
                label: sup_label,
                ptr: sup,
            });
            let (o0, o1) = h.alloc_dup(Term::Int(0));
            let sup_alias = unsafe { SupPtr::forge(sup_addr) };
            h.register_sup_at(o0.addr(), &sup_alias);

            // A different-label dup D over the sup node commutes it away,
            // minting two replacement &S sups — both re-registered to O.
            let (d0, d1) = h.alloc_dup_at(sup_node.into_addr());
            let d_label = h.dup_auto_label(d0);
            let exec = Executor::new(h, UnlimitedBudget);
            let na = h.alloc(Term::Dup {
                label: d_label,
                ptr: d0,
            });
            let nb = h.alloc(Term::Dup {
                label: d_label,
                ptr: d1,
            });
            let ra = rt.block_on(exec.whnf_at(na));
            let rb = rt.block_on(exec.whnf_at(nb));
            assert_eq!(h.arena_len(ArenaKind::Sups), 2);

            // Dropping O's side 1 (right) marks BOTH replacements and hands
            // back both right components for erasure.
            let DupDrop::Recorded { sup_sides } = h.dup_drop_side(o1) else {
                panic!("first drop must record");
            };
            assert_eq!(sup_sides.len(), 2);
            for dead in sup_sides {
                exec.erase(h.pull(dead));
            }
            // Both survivors (the marked sups) now read back as their left
            // components only: projections of the dup over 1.
            exec.erase(h.pull(ra));
            exec.erase(h.pull(rb));
            let DupDrop::Reclaim(v) = h.dup_drop_side(o0) else {
                panic!("second drop must reclaim");
            };
            exec.erase(h.pull(v));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Sups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn marked_sup_unwraps_at_whnf() {
        // A collapse-marked sup met by reduction unwraps to its surviving
        // component; the tombstone is erased and the cell freed.
        let rt = rt();
        let heap = Heap::new();
        heap.with(|h| {
            let one = h.alloc(Term::Int(1));
            let two = h.alloc(Term::Int(2));
            let sup = h.sup(one, two);
            let sup_addr = sup.addr();
            let label = LabelId::from_u56(h.intern_name("&S"));
            let sup_node = h.alloc(Term::Sup { label, ptr: sup });
            // Register to a dup cell and drop its right side: the sup collapses
            // to the left.
            let (o0, o1) = h.alloc_dup(Term::Int(0));
            let sup_alias = unsafe { SupPtr::forge(sup_addr) };
            h.register_sup_at(o0.addr(), &sup_alias);
            let exec = Executor::new(h, UnlimitedBudget);
            let o_label = h.dup_auto_label(o0);
            exec.erase(Term::Dup {
                label: o_label,
                ptr: o1,
            });
            // Reduce a term whose head is the marked sup: `&S{1,2} + 10` must
            // unwrap to 1 and evaluate to 11 (a live sup would BOP-SUP here).
            let ten = h.alloc(Term::Int(10));
            let bop = h.alloc(Term::Bop {
                op: BinaryOp::Add,
                lhs: sup_node,
                rhs: ten,
            });
            let result = rt.block_on(exec.normalize_at(bop));
            assert_eq!(format!("{}", Printer::new(h).pretty(&result)), "11");
            exec.erase(h.pull(result));
            // Cleanup the owner cell.
            let (v, dead) = h.try_dup_bypass(o0).unwrap();
            assert!(dead.is_empty());
            exec.erase(h.pull(v));
            assert_eq!(h.arena_len(ArenaKind::Dups), 0);
            assert_eq!(h.arena_len(ArenaKind::Sups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn erase_sup_reclaims_both_branches() {
        let heap = Heap::new();
        heap.with(|h| {
            let label = LabelId::from_u56(h.intern_name("&L"));
            let a = {
                let func = h.alloc(Term::Int(1));
                let arg = h.alloc(Term::Int(2));
                h.alloc(Term::App { func, arg })
            };
            let b = h.alloc(Term::Int(3));
            let sup = h.sup(a, b);
            let exec = Executor::new(h, UnlimitedBudget);
            exec.erase(Term::Sup { label, ptr: sup });
            assert_eq!(h.arena_len(ArenaKind::Sups), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }

    #[test]
    fn erase_mat_reclaims_table() {
        let heap = Heap::new();
        heap.with(|h| {
            let key = h.alloc(Term::Int(1));
            let branch = {
                let (binder, occ) = h.fresh_binder();
                let body = h.close_body(binder, occ);
                h.alloc(Term::Lam { body })
            };
            let default = h.alloc(Term::Int(0));
            let matches = h.alloc_match(MatchData {
                cases: vec![(key.into_addr(), branch.into_addr())],
                default: Some(default.into_addr()),
            });
            let exec = Executor::new(h, UnlimitedBudget);
            exec.erase(Term::Mat { matches });
            assert_eq!(h.arena_len(ArenaKind::Matches), 0);
            assert_eq!(h.arena_len(ArenaKind::Nodes), 0);
        });
    }
}
