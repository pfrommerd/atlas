use std::collections::HashSet;

use super::lang::{Var, Expr, Lambda, Primitive, LetIn, Bind, App, Invoke, Match, Case, Builtin};

pub trait FreeVariables {
    fn free_variables<'e>(&'e self, bound: &HashSet<&str>) -> HashSet<&'e str>;
}

impl FreeVariables for Primitive {
    fn free_variables<'e>(&'e self, _: &HashSet<&str>) -> HashSet<&'e str> {
        HashSet::new()
    }
}

impl FreeVariables for LetIn {
    fn free_variables<'e>(&'e self, bound: &HashSet<&str>) -> HashSet<&'e str> {
        let mut free = HashSet::new();
        let sub_bound = match &self.bind {
            Bind::NonRec(sym, val) => {
                free.extend(val.free_variables(bound));
                let mut sub_syms = bound.clone();
                sub_syms.insert(sym.name.as_str());
                sub_syms
            },
            Bind::Rec(binds) => {
                let mut sub_syms = bound.clone();
                for (sym, _) in binds {
                    sub_syms.insert(sym.name.as_str());
                }
                for (_, expr) in binds {
                    free.extend(expr.free_variables(&sub_syms));
                }
                sub_syms
            }
        };
        free.extend(self.body.free_variables(&sub_bound));
        free
    }
}

impl FreeVariables for Lambda {
    fn free_variables<'e>(&'e self, bound: &HashSet<&str>) -> HashSet<&'e str> {
        let mut sub_bound = bound.clone();
        for a in self.args.iter() {
            sub_bound.insert(a.name.as_str());
        }
        self.body.free_variables(&sub_bound)
    }
}

impl FreeVariables for Var {
    fn free_variables<'e>(&'e self, bound: &HashSet<&str>) -> HashSet<&'e str> {
        if bound.contains(self.name.as_str()) {
            HashSet::new()
        } else {
            let mut s = HashSet::new();
            s.insert(self.name.as_str());
            s
        }
    }
}

impl FreeVariables for App {
    fn free_variables<'e>(&'e self, bound: &HashSet<&str>) -> HashSet<&'e str> {
        let mut free = self.lam.free_variables(bound);
        for a in &self.args {
            free.extend(a.free_variables(bound));
        }
        free
    }
}

impl FreeVariables for Match {
    fn free_variables<'e>(&'e self, bound: &HashSet<&str>) -> HashSet<&'e str> {
        let mut free = self.scrut.free_variables(bound);
        let sub_bound = match &self.bind {
            Some(s) => {
                let mut sub = bound.clone();
                sub.insert(s.name.as_str());
                sub
            },
            None => bound.clone()
        };
        for c in self.cases.iter() {
            free.extend(match c {
                Case::Eq(_, e) => e.free_variables(&sub_bound),
                Case::Tag(_, e) => e.free_variables(&sub_bound)
            });
        }
        free
    }
}

impl FreeVariables for Builtin {
    fn free_variables<'e>(&'e self, bound: &HashSet<&str>) -> HashSet<&'e str> {
        let mut free = HashSet::new();
        for a in self.args.iter() {
            free.extend(a.free_variables(bound));
        }
        free
    }
}

impl FreeVariables for Invoke {
    fn free_variables<'e>(&'e self, bound: &HashSet<&str>) -> HashSet<&'e str> {
       self.target.free_variables(bound)
    }
}

impl FreeVariables for Expr {
    fn free_variables<'e>(&'e self, bound: &HashSet<&str>) -> HashSet<&'e str> {
        use Expr::*;
        match self {
            Var(v) => v.free_variables(bound),
            Lambda(l) => l.free_variables(bound),
            Primitive(p) => p.free_variables(bound),
            LetIn(l) => l.free_variables(bound),
            App(a) => a.free_variables(bound),
            Invoke(i) => i.free_variables(bound),
            Match(m) => m.free_variables(bound),
            Builtin(b) => b.free_variables(bound)
        }
    }
}