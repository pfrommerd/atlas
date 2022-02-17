use std::collections::HashSet;
use std::vec;

use codespan::Span;

use super::ast::{Expr as AExpr, Literal}; // this is probably bad idk
use super::ast; 

use crate::core::lang::{Expr as CExpr, Primitive};
use crate::core::lang;


impl ast::Literal {
    pub fn transpile(&self) -> lang::Primitive {
        match &*self {
            ast::Literal::Unit => lang::Primitive::Unit,
            ast::Literal::Bool(b) => lang::Primitive::Bool(*b),
            ast::Literal::Int(i) => lang::Primitive::Int(*i),
            ast::Literal::Float(f) => lang::Primitive::Float(**f),
            ast::Literal::String(s) => lang::Primitive::String(s.to_string()),
            ast::Literal::Char(c) => lang::Primitive::Char(*c),
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
            let cons = lang::Builtin{op: "__cons".to_string(), args: vec![hd.transpile(), rest]};
            CExpr::Builtin(cons)
        }
        None => {
            lang::Expr::Primitive(lang::Primitive::EmptyList)
        }
    }
}

fn transpile_if_else(scrut: &Box<AExpr>, if_case: &Box<AExpr>, else_case: &Option<Box<AExpr>>) -> CExpr {    
    let if_branch = lang::Case::Eq(CExpr::Primitive(Primitive::Bool(true)), if_case.transpile());
    
    let unit = &Box::new(AExpr::Literal(Span::new(0,0), Literal::Unit));
    let else_val = match else_case {
        Some(v) => v,
        None => unit
    };
    let else_branch = lang::Case::Eq(CExpr::Primitive(Primitive::Bool(false)), else_val.transpile());

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

    pub fn globals(&self) -> Vec<&str> {
        if self.mods.contains(&ast::DeclareModifier::Pub) {
            match self.pattern {
                ast::Pattern::Identifier(_, name) => vec![name],
                _ => todo!()
            }
        } else {
            return Vec::new();
        }
    }
}


impl<'src> ast::FnDeclare<'src> {
    pub fn transpile(&self) -> lang::Bind {
        todo!()
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
            ast::Declaration::Fn(_) => todo!(),
        }
    }

    pub fn globals(&self) -> HashSet<&str> {
        match self {
            ast::Declaration::Let(_) => todo!(),
            ast::Declaration::Block(b) => b.globals(),
            ast::Declaration::Fn(_) => todo!(),
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

        let key = CExpr::Primitive(lang::Primitive::String(name.to_string()));
        let val = exp.transpile();

        let insert_call = lang::Builtin{op: "__insert".to_string(), args: vec![key, val, rest]};
        return CExpr::Builtin(insert_call)
    } else {
        return CExpr::Primitive(lang::Primitive::EmptyRecord)
    }
}

fn transpile_tuple(items: &Vec<AExpr>) -> CExpr {
    if let Some((hd, tl)) = items.split_first() {
        let rest = transpile_tuple(&tl.to_vec());
        let append = lang::Builtin{op: "__append".to_string(), args: vec![hd.transpile(), rest]};
        CExpr::Builtin(append)
    } else {
        CExpr::Primitive(lang::Primitive::EmptyTuple)
    }
}


impl<'src> ast::Expr<'src> {
    pub fn transpile(&self) -> lang::Expr {
        match self {
            ast::Expr::Identifier(_, name) => 
                lang::Expr::Var(lang::Symbol{name: name.to_string()}),
            ast::Expr::Literal(_, l) => 
                lang::Expr::Primitive(l.transpile()),
            ast::Expr::List(_, items) => 
                transpile_list(items),
            ast::Expr::Tuple(_, items) => 
                transpile_tuple(items),
            ast::Expr::Record(_, fields) => 
                transpile_record(fields),
            ast::Expr::Prefix(_, _, _) => todo!(),
            ast::Expr::Infix(_, _, _) => todo!(),
            ast::Expr::Call(_, fun, args) => 
                transpile_call(fun, args),
            ast::Expr::Scope(ast::Scope{span: _, decl, expr}) => 
                transpile_scope(decl, expr),
            ast::Expr::Lambda(_, _params, _body) => {
                todo!()
            },
            ast::Expr::IfElse(_, scrutinized, if_case, else_case) => 
                transpile_if_else(scrutinized, if_case, else_case),
            ast::Expr::Project(_, _v, _proj) => {
                todo!()
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