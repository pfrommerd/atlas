use std::collections::HashSet;
use std::vec;

use codespan::Span;
use lang::Builtin;

use super::ast::{Expr as AExpr}; // this is probably bad idk
use super::ast; 

use crate::core::lang::{Expr as CExpr};
use crate::core::lang;


impl ast::Literal {
    pub fn transpile(&self) -> lang::Literal {
        match &*self {
            ast::Literal::Unit => lang::Literal::Unit,
            ast::Literal::Bool(b) => lang::Literal::Bool(*b),
            ast::Literal::Int(i) => lang::Literal::Int(*i),
            ast::Literal::Float(f) => lang::Literal::Float(**f),
            ast::Literal::String(s) => lang::Literal::String(s.to_string()),
            ast::Literal::Char(c) => lang::Literal::Char(*c),
        }
    }
}

fn span (e: ast::Expr) -> Span {
        match e {
            ast::Expr::Identifier(s, _) => s,
            ast::Expr::Literal(s, _) => s,
            ast::Expr::List(s, _) => s,
            ast::Expr::Tuple(s, _) => s,
            ast::Expr::Record(s, _) => s,
            ast::Expr::Prefix(s, _, _) => s,
            ast::Expr::Infix(s, _, _) => s,
            ast::Expr::Call(s, _, _) => s,
            ast::Expr::Scope(ast::Scope{span, ..}) => span,
            ast::Expr::Lambda(s, _, _) => s,
            ast::Expr::IfElse(s, _, _, _) => s,
            ast::Expr::Project(s, _, _) => s,
            ast::Expr::Match(s, _, _) => s,
            ast::Expr::Module(ast::Module{span, ..}) => span,
            ast::Expr::Builtin(s, _, _) =>s,
        }
}


fn transpile_list<'src>(
    items: &Vec<ast::Expr<'src>>
) -> lang::Expr {
    match items.split_first() {
        Some((hd, tl)) => {
            let rest = transpile_list(&tl.to_vec());
            let cons = lang::Builtin{op: "cons".to_string(), args: vec![hd.transpile(), rest]};
            CExpr::Builtin(cons)
        }
        None => {
            CExpr::Builtin(Builtin{op: "empty_list".to_string(), args: Vec::new()})
        }
    }
}

fn transpile_if_else(scrut: &Box<AExpr>, if_case: &Box<AExpr>, else_case: &Option<Box<AExpr>>) -> CExpr {    
    let if_branch = lang::Case::Eq(lang::Primitive::Bool(true), if_case.transpile());
    
    let unit = &Box::new(AExpr::Literal(Span::new(0,0), ast::Literal::Unit));
    let else_val = match else_case {
        Some(v) => v,
        None => unit
    };
    let else_branch = lang::Case::Eq(lang::Primitive::Bool(false), else_val.transpile());

    let m = lang::Match{scrut: Box::new(scrut.transpile()), bind: None, cases: vec![if_branch, else_branch]};
    CExpr::Match(m)
}

fn transpile_call(func: &Box<AExpr>, args: &Vec<ast::Arg>) -> CExpr {
    let mut t_args = Vec::new();
    
    for a in args {
        match a {
            ast::Arg::Pos(_, val) => t_args.push(val.transpile()),
            _ => todo!()
        }
    }

    let app = lang::Expr::App(lang::App{lam: Box::new(func.transpile()), args: t_args});

    return CExpr::Invoke(lang::Invoke{target:Box::new(app)})
}


impl<'src> ast::LetDeclare<'src> {
    pub fn transpile(&self) -> lang::Bind {
        match self.pattern {
            ast::Pattern::Identifier(_, name) => 
                return lang::Bind::NonRec(lang::Symbol{name: name.to_string()}, Box::new(self.binding.transpile())),
            _ => todo!()
        }
    }

    pub fn globals(&self) -> HashSet<&str> {
        if self.mods.contains(&ast::DeclareModifier::Pub) {
            match self.pattern {
                ast::Pattern::Identifier(_, name) => HashSet::from([name]),
                _ => todo!()
            }
        } else {
            return HashSet::new();
        }
    }
}


impl<'src> ast::FnDeclare<'src> {
    pub fn transpile(&self) -> lang::Bind {
        let lam = transpile_lambda(&self.params, &self.scope);
        lang::Bind::NonRec(lang::Symbol{name: self.name.to_string()}, Box::new(lam))
    }

    pub fn globals(&self) -> HashSet<&str> {
        if self.mods.contains(&ast::DeclareModifier::Pub) {
            return HashSet::from([self.name]);
        } else {
            return HashSet::new();
        }
    }
}


impl<'src> ast::BlockDeclare<'src> {
    pub fn transpile(&self) -> lang::Bind {
        todo!()
    }

    pub fn globals(&self) -> HashSet<&str> {
        if self.mods.contains(&ast::DeclareModifier::Pub) {
            self.decls.iter().flat_map(|d| d.globals()).collect()
        } else {
            HashSet::new()
        }
    }
}

impl<'src> ast::Declaration<'src> {
    pub fn transpile(&self) -> lang::Bind {
        match self {
            ast::Declaration::Let(ld) => {
                return ld.transpile()
            },
            ast::Declaration::Block(_) => todo!(),
            ast::Declaration::Fn(fd) => fd.transpile(),
        }
    }

    pub fn globals(&self) -> HashSet<&str> {
        match self {
            ast::Declaration::Let(ld) => ld.globals(),
            ast::Declaration::Block(b) => b.globals(),
            ast::Declaration::Fn(fd) => fd.globals(),
        }
    }
}


fn transpile_scope(decls: &Vec<ast::Declaration>, exp: &Box<AExpr>) -> CExpr {
    if let Some((hd, tl)) = decls.split_first() {
        let rest = Box::new(transpile_scope(&tl.to_vec(), exp));
        return CExpr::LetIn(lang::LetIn{bind: hd.transpile(), body: rest})
    } else {
        return exp.transpile()
    }
}



impl<'src> ast::Module<'src> {
    pub fn globals(&self) -> HashSet<&str> {
        self.decl.iter().flat_map(|d| d.globals()).collect()
    }

    pub fn transpile(&self) -> CExpr {
        let record_fields = self
        .globals()
        .iter()
        .map(|s| 
            ast::Field::Simple(Span::new(0, 0), s, AExpr::Identifier(Span::new(0, 0), s))
        ).collect();

        let record = Box::new(AExpr::Record(self.span, record_fields));

        transpile_scope(&self.decl, &record)
    }
}


fn transpile_record(fields: &Vec<ast::Field>) -> CExpr {
    if let Some((hd, tl)) = fields.split_first() {
        let rest = transpile_record(&tl.to_vec());
        let (name, exp) = match hd {
            ast::Field::Simple(_, name, exp) => (name, exp),
            _ => todo!()
        };

        let key = CExpr::Literal(lang::Literal::String(name.to_string()));
        let val = exp.transpile();

        let insert_call = lang::Builtin{op: "insert".to_string(), args: vec![rest, key, val]};
        return CExpr::Builtin(insert_call)
    } else {
        return CExpr::Builtin(lang::Builtin{op: "insert".to_string(), args: vec![]})
    }
}

fn transpile_tuple(items: &Vec<AExpr>) -> CExpr {
    if let Some((hd, tl)) = items.split_first() {
        let rest = transpile_tuple(&tl.to_vec());
        let append = lang::Builtin{op: "append".to_string(), args: vec![hd.transpile(), rest]};
        CExpr::Builtin(append)
    } else {
        CExpr::Builtin(Builtin{op: "empty_tuple".to_string(), args: Vec::new()})
    }
}

fn transpile_lambda(params: &Vec<ast::Parameter>, body: &AExpr) -> CExpr {
    let args = 
        params.iter()
              .map(|ast::Parameter::Named(_, name)| lang::Symbol{name: name.to_string()})
              .collect();
    
    let lam =  lang::Lambda{args, body: Box::new(body.transpile())};
    return CExpr::Lambda(lam)
}


pub fn symbol_priority(sym: &str) -> u8 {
    match sym {
        "-" => 0,
        "+" => 0,
        "*" => 1,
        "/" => 1,
        _ => 2,
    }
}

pub fn transpile_infix(args: &Vec<AExpr<'_>>, ops: &Vec<&str>) -> CExpr {
    if args.len() == 1 && ops.len() == 0 {
        // just transpile as per normal
        return args[0].transpile()
    }
    if args.len() < 2 {
        panic!();
    }
    // First we find the rightmost, lowest-priority operation
    // to split on
    let mut lowest_priority: u8 = 255;
    let mut split_idx = 0;
    for (idx, op) in ops.iter().enumerate() {
        let p = symbol_priority(op);
        if p <= lowest_priority {
            lowest_priority = p;
            split_idx = idx;
        }
    }

    // Get the left and right arguments
    // TODO: Make more efficient by using immutable slices rather than
    // vectors
    let mut largs = args.clone();
    let rargs = largs.split_off(split_idx + 1);

    let mut lops = ops.clone();
    let mut rops = lops.split_off(split_idx);
    let op= rops.pop().unwrap();

    let op_exp = CExpr::Var(lang::Symbol{name: op.to_string()});

    let args = vec![transpile_infix(&largs, &lops), transpile_infix(&rargs, &rops)];

    let app_exp = CExpr::App(lang::App{lam: Box::new(op_exp), args});

    CExpr::Invoke(lang::Invoke{target: Box::new(app_exp)})
}

impl<'src> ast::Expr<'src> {
    pub fn transpile(&self) -> lang::Expr {
        match self {
            ast::Expr::Identifier(_, name) => 
                lang::Expr::Var(lang::Symbol{name: name.to_string()}),
            ast::Expr::Literal(_, l) => 
                CExpr::Literal(l.transpile()),
            ast::Expr::List(_, items) => 
                transpile_list(items),
            ast::Expr::Tuple(_, items) => 
                transpile_tuple(items),
            ast::Expr::Record(_, fields) => 
                transpile_record(fields),
            ast::Expr::Prefix(_, _, _) => todo!(),
            ast::Expr::Infix(_, args, ops) => 
                transpile_infix(args, ops),
            ast::Expr::Call(_, fun, args) => 
                transpile_call(fun, args),
            ast::Expr::Scope(ast::Scope{span: _, decl, expr}) => 
                transpile_scope(decl, expr),
            ast::Expr::Lambda(_, params, body) => 
                transpile_lambda(&params, body),
            ast::Expr::IfElse(_, scrutinized, if_case, else_case) => 
                transpile_if_else(scrutinized, if_case, else_case),
            ast::Expr::Project(_, v, proj) => {
                let p = CExpr::Literal(lang::Literal::String(proj.to_string()));
                let projection = lang::Builtin{op: "__project".to_string(), args: vec![v.transpile(), p]};
                CExpr::Builtin(projection)
            }
            ast::Expr::Match(_, _, _) => todo!(),
            ast::Expr::Module(m) => m.transpile(),
            ast::Expr::Builtin(_, op, args) => {
                CExpr::Builtin(
                    lang::Builtin{
                        op: op.to_string(), 
                        args: args.iter().map(|a| a.transpile()).collect()
                    }
                )
            },
        }
    }
}