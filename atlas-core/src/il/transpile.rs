use atlas_parse::ast::{
    Expr as AstExpr,
    Literal as AstLiteral,
    Tuple, Record, List, Infix,
    IfElse, Match, ExprBlock
};

use super::{Expr, Invoke, Literal};

pub trait Transpile {
    fn transpile(&self) -> Expr;
}

impl Transpile for AstExpr<'_> {
    fn transpile(&self) -> Expr {
        use AstExpr::*;
        match self {
            Literal(l) => l.transpile(),
            Identifier(s) => Expr::Var(s.to_string()),
            Tuple(t) => t.transpile(),
            Record(r) => r.transpile(),
            List(l) => l.transpile(),
            IfElse(e) => e.transpile(),
            Match(m) => m.transpile(),
            Block(b) => b.transpile(),
            Infix(infix) => infix.transpile(),
            Unary(op, exp) => {
                Expr::Invoke(Box::new(Invoke {
                    lam: Expr::Var(op.to_string()),
                    args: vec![exp.transpile()]
                }))
            },
            Project(exp, field) => {
                Expr::Invoke(Box::new(Invoke {
                    lam: Expr::Var("__project__".to_string()),
                    args: vec![exp.transpile(), 
                    Expr::Literal(super::Literal::String(field.to_string()))]
                }))
            },
            Index(exp, val) => {
                Expr::Invoke(Box::new(Invoke {
                    lam: Expr::Var("__index__".to_string()),
                    args: vec![exp.transpile(), val.transpile()]
                }))
            },
            Call(exp, args) => {
                Expr::Invoke(Box::new(Invoke {
                    lam: exp.transpile(),
                    args: args.iter().map(|x| x.transpile()).collect()
                }))
            },
        }
    }
}

impl Transpile for AstLiteral<'_> {
    fn transpile(&self) -> Expr {
        use AstLiteral::*;
        Expr::Literal(match self {
            &Integer(i) => Literal::Integer(i),
            Float(f) => Literal::Float(f.into_inner()),
            &Bool(b) => Literal::Bool(b),
            String(s) => Literal::String(s.to_string()),
            Unit => Literal::Unit
        })
    }
}

fn infix_op_priority(s: &str) -> i8 {
    match s {
        "*" => 2,
        "/" => 2,
        "-" => 1,
        "+" => 1,
        _ => 0,
    }
}

fn transpile_infix(s: Infix<'_>) -> Expr {
    // If there is only 1 set of arguments,
    // just emit the call directly
    if s.rhs.len() == 0 {
        s.lhs.transpile()
    } else if s.rhs.len() == 1 {
        let (op, rhs) = s.rhs.iter().next().unwrap();
        Expr::Invoke(Box::new(Invoke {
            lam: Expr::Literal(Literal::String(op.to_string())),
            args: vec![s.lhs.transpile(), rhs.transpile()]
        }))
    } else {
        let mut lowest_priority = std::i8::MAX;
        let mut lowest_idx = 0;
        for (idx, (op, _)) in s.rhs.iter().enumerate() {
            let priority = infix_op_priority(op);
            if priority  <= lowest_priority {
                lowest_idx = idx;
                lowest_priority = priority;
            }
        }
        let mut lhs = s.rhs;
        let rhs = lhs.split_off(lowest_idx + 1);
        let (sop, rhs_lhs) = lhs.pop().unwrap();

        // Do things
        let rhs_infix = Infix { lhs: rhs_lhs, rhs };
        let lhs_infix = Infix { lhs: s.lhs, rhs: lhs };
        Expr::Invoke(Box::new(Invoke {
            lam: Expr::Literal(Literal::String(sop.to_string())),
            args: vec![transpile_infix(lhs_infix), transpile_infix(rhs_infix)]
        }))
    }
}

// The infix transpilation is a bit complicated
// since it needs to encode order-of-ops
impl Transpile for Infix<'_> {
    fn transpile(&self) -> Expr {
        transpile_infix(self.clone())
    }
}

impl Transpile for Tuple<'_> {
    fn transpile(&self) -> Expr {
        todo!()
    }
}

impl Transpile for Record<'_> {
    fn transpile(&self) -> Expr {
        todo!()
    }
}

impl Transpile for List<'_> {
    fn transpile(&self) -> Expr {
        todo!()
    }
}

impl Transpile for IfElse<'_> {
    fn transpile(&self) -> Expr {
        todo!()
    }
}

impl Transpile for Match<'_> {
    fn transpile(&self) -> Expr {
        todo!()
    }
}

impl Transpile for ExprBlock<'_> {
    fn transpile(&self) -> Expr {
        todo!()
    }
}