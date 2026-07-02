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
        // Used N times -> a single dup with N projection wires (each use is a
        // distinct `Ref`); the executor fires all wires at once.
        assert_eq!(run(r"(\&x -> x + x + x) 2").unwrap(), "6");
        assert_eq!(run(r"(\&x -> x + x + x + x) 3").unwrap(), "12");
        assert_eq!(run(r"(\&x -> x * x * x * x) 2").unwrap(), "16");
    }

    #[test]
    fn explicit_n_way_dup() {
        // `&{a b c}` binds three projection wires of one dup over the value.
        assert_eq!(run(r"&{a b c} = 5; (a + b) + c").unwrap(), "15");
        assert_eq!(run(r"&{a b c d e} = 2; ((((a + b) + c) + d) + e)").unwrap(), "10");
    }

    #[test]
    fn dup_over_lambda_n_ways() {
        // A cloned lambda used three times is one 3-way dup over the lambda
        // (DUP-LAM with three wires), each copy applied to its own argument.
        assert_eq!(
            run(r"&f = \y -> y + 1; ((f 1) + (f 2)) + (f 3)").unwrap(),
            "9"
        );
    }

    #[test]
    fn nested_clones_combine() {
        // `&y = x` duplicates a projection of the `&x` dup: the two dups combine
        // into one fan over the value, so all uses see the same `10`.
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
        // Three uses are a single dup with three projection wires (no chaining).
        assert_eq!(
            run(r"\&x -> x + x + x").unwrap(),
            "&{a, b, c} = d;\n\\d -> ((a + b) + c)"
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
    use super::run;

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
        assert_eq!(
            run(&format!("{opt}(Opt (type ()))::None")).unwrap(),
            "None"
        );
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
        assert_eq!(
            run(r"(type (type (), type ()))::New 1").unwrap(),
            "(New 1)"
        );
        // A variant selector (awaiting args) is a value that prints as its name.
        let opt = r"Opt = \ T -> type { Some(T), None }; ";
        assert_eq!(
            run(&format!("{opt}(Opt (type ()))::Some")).unwrap(),
            "Some"
        );
        // A multi-arg variant gathers args; partial then complete.
        let pair = r"Pair = \ A B -> type { Both(A, B) }; ";
        assert_eq!(
            run(&format!("{pair}(Pair (type ()) (type ()))::Both 1")).unwrap(),
            "(Both 1)"
        );
        assert_eq!(
            run(&format!(
                "{pair}(Pair (type ()) (type ()))::Both 1 true"
            ))
            .unwrap(),
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
}
