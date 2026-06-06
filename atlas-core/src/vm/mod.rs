pub mod heap;
pub mod term;

use std::collections::HashMap;

use crate::core::expr::{self, Expr, Pat};
use heap::{Heap, MatchData, PatKey};
use term::{BinaryOp, Label, MatchId, NameId, Term, TermPtr, TermValue};

/// Default interaction budget for [`run`].
pub const DEFAULT_BUDGET: u64 = 50_000_000;

/// Parse, desugar, evaluate, and pretty-print a single source expression.
pub fn run(src: &str) -> Result<String, String> {
    let node = crate::core::parse::parse(src)?;
    let expr = expr::desugar(&node)?;
    let mut heap = Heap::new();
    let root = lower(&mut heap, &expr, &mut Vec::new())?;
    let result = heap.normalize(root, DEFAULT_BUDGET);
    Ok(Printer::new(&heap).show(result))
}

// ========================================================================
// Lowering: desugared Expr (de Bruijn) -> heap interaction terms
// ========================================================================

/// A binder currently in scope while lowering, indexed by de Bruijn level.
enum Frame {
    /// a lambda binder slot (`Var` resolves to `Var(loc)`)
    Lam(u64),
    /// a duplication node + its (interned) label
    Dup(u64, u16),
}

// ========================================================================
// Readback / printing
// ========================================================================

struct Printer<'a> {
    heap: &'a Heap,
    names: HashMap<u64, String>,
    dups: HashMap<u64, String>,
    counter: usize,
}

impl<'a> Printer<'a> {
    fn new(heap: &'a Heap) -> Self {
        Printer {
            heap,
            names: HashMap::new(),
            dups: HashMap::new(),
            counter: 0,
        }
    }

    fn fresh(&mut self) -> String {
        let n = self.counter;
        self.counter += 1;
        let letter = (b'a' + (n % 26) as u8) as char;
        if n < 26 {
            letter.to_string()
        } else {
            format!("{}{}", letter, n / 26)
        }
    }

    fn var_name(&mut self, loc: u64) -> String {
        if let Some(s) = self.names.get(&loc) {
            return s.clone();
        }
        let s = self.fresh();
        self.names.insert(loc, s.clone());
        s
    }

    fn dup_name(&mut self, d: u64) -> String {
        if let Some(s) = self.dups.get(&d) {
            return s.clone();
        }
        let s = self.fresh();
        self.dups.insert(d, s.clone());
        s
    }

    fn show(&mut self, t: Term) -> String {
        match t.unpack() {
            TermValue::Lam(p) => {
                let nm = self.var_name(p.0);
                let body = self.heap.get(p.0 + 1);
                format!("\\{} -> {}", nm, self.show(body))
            }
            TermValue::App(p) => {
                let f = self.heap.get(p.0);
                let x = self.heap.get(p.0 + 1);
                format!("({} {})", self.show(f), self.show(x))
            }
            TermValue::Var(p) => {
                let s = self.heap.get(p.0);
                if s.is_sub() {
                    self.show(s.clear_sub())
                } else {
                    self.var_name(p.0)
                }
            }
            TermValue::Dp0 { ptr, .. } => self.show_dup(ptr.0, 1, "0"),
            TermValue::Dp1 { ptr, .. } => self.show_dup(ptr.0, 2, "1"),
            TermValue::Sup { label, ptr } => {
                let lab = self.heap.name(label.0).to_string();
                let a = self.heap.get(ptr.0);
                let b = self.heap.get(ptr.0 + 1);
                format!("&{}{{{}, {}}}", lab, self.show(a), self.show(b))
            }
            TermValue::Num(n) => format!("{}", n),
            TermValue::Ctr { name, arity, ptr } => self.show_ctr(name, arity, ptr),
            TermValue::Wld => "*".to_string(),
            TermValue::Bop { op, ptr } => {
                let l = self.heap.get(ptr.0);
                let r = self.heap.get(ptr.0 + 1);
                format!("({} {} {})", self.show(l), op_sym(op), self.show(r))
            }
            TermValue::Mat(_) => "?{...}".to_string(),
            _ => "<?>".to_string(),
        }
    }

    /// A free (unsubstituted) duplication projection, or its substitution.
    fn show_dup(&mut self, d: u64, off: u64, suffix: &str) -> String {
        let s = self.heap.get(d + off);
        if s.is_sub() {
            self.show(s.clear_sub())
        } else {
            format!("{}.{}", self.dup_name(d), suffix)
        }
    }

    fn show_ctr(&mut self, name: NameId, arity: u8, ptr: TermPtr) -> String {
        let nm = self.heap.name(name.0).to_string();
        let arity = arity as usize;
        // list sugar
        if nm == "Nil" && arity == 0 {
            return "[]".to_string();
        }
        if nm == "Con" && arity == 2 {
            let mut items = Vec::new();
            let mut cell = ptr.0;
            loop {
                let head = self.heap.get(cell);
                items.push(self.show(head));
                let tail = self.heap.get(cell + 1);
                match tail.unpack() {
                    TermValue::Ctr { name, arity, ptr }
                        if self.heap.name(name.0) == "Con" && arity == 2 =>
                    {
                        cell = ptr.0;
                    }
                    TermValue::Ctr { name, .. } if self.heap.name(name.0) == "Nil" => {
                        return format!("[{}]", items.join(", "));
                    }
                    _ => {
                        // improper list: fall back
                        items.push(self.show(tail));
                        return format!("[{}]", items.join(", "));
                    }
                }
            }
        }
        if arity == 0 {
            format!("#{}", nm)
        } else {
            let mut fields: Vec<String> = Vec::with_capacity(arity);
            for i in 0..arity {
                let f = self.heap.get(ptr.0 + i as u64);
                fields.push(self.show(f));
            }
            format!("#{}{{{}}}", nm, fields.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eval(src: &str) -> String {
        run(src).unwrap_or_else(|e| panic!("eval `{src}` failed: {e}"))
    }

    fn eval_budget(src: &str, budget: u64) -> (String, u64) {
        let node = crate::core::parse::parse(src).unwrap();
        let expr = expr::desugar(&node).unwrap();
        let mut heap = Heap::new();
        let root = lower(&mut heap, &expr, &mut Vec::new()).unwrap();
        let result = heap.normalize(root, budget);
        (Printer::new(&heap).show(result), heap.itrs)
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
        assert_eq!(eval(r"(\x y -> x) (\a -> a)"), r"\a -> \b -> b");
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
        assert_eq!(eval(r"(\_ -> *) 5"), "*");
    }
}
