use ordered_float::NotNan;

use crate::core::lang::{
    ExprBuilder, PrimitiveBuilder, SymbolMap, CaseBuilder, ParamBuilder, BindsBuilder, BindBuilder
};
use std::ops::Deref;
pub use codespan::{ByteIndex, ByteOffset, ColumnIndex, ColumnOffset, LineIndex, LineOffset, Span};

use pretty::{DocBuilder, DocAllocator, Doc};

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Literal {
    Unit,
    Bool(bool),
    Int(i64),
    Float(NotNan<f64>),
    String(String),
    Char(char),
}

impl Literal {
    pub fn transpile(&self, b: PrimitiveBuilder) {
        let mut pb = b;
        use Literal::*;
        match self {
            Unit => pb.set_unit(()),
            Bool(b) => pb.set_bool(*b),
            Int(i) => pb.set_int(*i),
            Float(f) => pb.set_float(f.into_inner()),
            String(s) => pb.set_string(s),
            Char(c) => pb.set_char((*c) as u32)
        }
    }
}

impl Literal {
    pub fn pretty<'b, D, A>(&'b self, allocator: &'b D) -> DocBuilder<'b, D, A> 
    where
        D: DocAllocator<'b, A>,
        D::Doc: Clone,
        A: Clone,
    {
        match &*self {
            Literal::Unit => allocator.text("unit"),
            Literal::Bool(b) => allocator.as_string(b),
            Literal::Int(i) => allocator.as_string(i),
            Literal::Float(f) => allocator.as_string(f),
            Literal::String(s) => allocator.text(s).double_quotes(),
            Literal::Char(c) => allocator.as_string(c).single_quotes()
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum PrimitiveType {
    Unit,
    Bool,
    Int,
    Float,
    Char,
    String,
    Buffer,
}

// Fields that come later override fields that come earlier
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Field<'src> {
    Shorthand(Span, &'src str),
    Simple(Span, &'src str, Expr<'src>), // a : 0
    Expansion(Span, Expr<'src>),         // ...b
}

impl<'src> Field<'src> {
    pub fn pretty<'a, D, A>(&'a self, allocator: &'a D) -> DocBuilder<'a, D, A>
    where
        D: DocAllocator<'a, A>,
        D::Doc: Clone,
        A: Clone,
    {
        match *self {
            Field::Shorthand(_, ref s) => 
                allocator.text("pos_field ").append(allocator.text(*s)),
            Field::Simple(_, name, ref val) => 
                allocator.text("field ").append(allocator.text(name)).append(val.pretty(allocator)),
            Field::Expansion(_, ref val) => 
                allocator.text("field_expansion ").append(val.pretty(allocator)),
        }.parens().group()
    }
}

// Patterns

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum FieldPattern<'src> {
    Shorthand(Span, &'src str),             // as in {a, b, c},
    Simple(Span, &'src str, Pattern<'src>), // as in {a: (a, b)}, will bind a, b
    Expansion(Span, Option<&'src str>),     // {...bar} or {a, ...}
}

impl<'src> FieldPattern<'src> {
    pub fn pretty<'a, D, A>(&'a self, allocator: &'a D) -> DocBuilder<'a, D, A>
    where
        D: DocAllocator<'a, A>,
        D::Doc: Clone,
        A: Clone,
    {
        match *self {
            FieldPattern::Shorthand(_, ref s) => 
                allocator.text("field-pattern-pos ").append(allocator.text(*s)),
            FieldPattern::Simple(_, name, ref pat) => 
                allocator.text("field-pattern ").append(allocator.text(name)).append(pat.pretty(allocator)),
            FieldPattern::Expansion(_, None) => 
                allocator.text("field-pattern-expansion-unnamed"),
            FieldPattern::Expansion(_, Some(name)) => 
                allocator.text("field-pattern-expansion-named ").append(allocator.as_string(name)),
        }.parens().group()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ListItemPattern<'src> {
    Simple(Span, Pattern<'src>),
    Expansion(Span, Option<&'src str>)
}


impl<'src> ListItemPattern<'src> {
    pub fn pretty<'a, D, A>(&'a self, allocator: &'a D) -> DocBuilder<'a, D, A>
    where
        D: DocAllocator<'a, A>,
        D::Doc: Clone,
        A: Clone,
    {
        match *self {
            ListItemPattern::Simple(_, ref pat) => 
                allocator.text("list-pattern ").append(pat.pretty(allocator)).parens(),
            ListItemPattern::Expansion(_, None) => 
                allocator.text("list-pattern-expansion-unnamed"),
            ListItemPattern::Expansion(_, Some(name)) => 
                allocator.text("list-pattern-expansion-named ").append(allocator.as_string(name)).parens(),
        }.group()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Pattern<'src> {
    Hole(Span), // _
    Identifier(Span, &'src str),
    Literal(Span, Literal),
    Tuple(Span, Vec<Pattern<'src>>),
    List(Span, Vec<ListItemPattern<'src>>), 
    Record(Span, Vec<FieldPattern<'src>>),
    Variant(Span, &'src str, Vec<Pattern<'src>>),
    Of(Span, PrimitiveType, &'src str), // int(a), float(b), etc. allows matching by type
}

impl<'src> Pattern<'src> {
    pub fn pretty<'a, D, A>(&'a self, allocator: &'a D) -> DocBuilder<'a, D, A>
    where
        D: DocAllocator<'a, A>,
        D::Doc: Clone,
        A: Clone,
    {
        match *self {
            Pattern::Hole(_) => allocator.text("pattern-hole"),
            Pattern::Identifier(_, name) => 
                allocator.text("pattern-identifier ").append(allocator.text(name)).parens(),
            Pattern::Literal(_, ref lit) => 
                allocator.text("pattern-literal ").append(lit.pretty(allocator)).parens(),
            Pattern::Tuple(_,ref patterns) => 
                allocator.text("pattern-tuple ")
                         .append(
                            allocator.intersperse(
                                patterns.iter().map(|p| p.pretty(allocator).parens()), 
                                Doc::space()
                            )
                         ).parens(),
            _ => todo!()
        }.group()
    }
}

// Argument types

// Parameter is for the declaration, arg is for the call
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Parameter<'src> {
    Named(Span, &'src str), // fn foo(a)
    Optional(Span, &'src str),
    VarPos(Span, Option<&'src str>), // fn foo(..a)
    VarKeys(Span, Option<&'src str>), // fn foo(...a)
}

impl<'src> Parameter<'src> {
    pub fn pretty<'a, D, A>(&'a self, allocator: &'a D) -> DocBuilder<'a, D, A>
    where
        D: DocAllocator<'a, A>,
        D::Doc: Clone,
        A: Clone,
    {
        match *self {
            Parameter::Named(_, name) => 
                allocator.text("param-named ").append(allocator.text(name)).parens(),
            Parameter::Optional(_, name) =>
                allocator.text("param-optional ").append(allocator.text(name)).parens(),
            Parameter::VarPos(_, None) =>
                allocator.text("param-variable-positional-nameless"),
            Parameter::VarPos(_, Some(name)) =>
                allocator.text("param-variable-positional-named ").append(allocator.text(name)).parens(),
            Parameter::VarKeys(_, None) => 
                allocator.text("param-variable-keys-nameless"),
            Parameter::VarKeys(_, Some(name)) => 
                allocator.text("param-variable-keys-named ").append(name).parens()
        }.group()
    }
}

impl<'src> Parameter<'src> {
    // pub fn transpile<'a>(&self, env: &'a SymbolMap, builder: ParamBuilder<'_>) -> SymbolMap<'a> {
    //     match self {
    //         Parameter::Named(_, name) => {
    //             let sym  = builder.init_symbol();
    //             new_env.add(name)
    //         },
    //         // right now only positional args
    //         Parameter::Optional(_, _) => todo!(),
    //         Parameter::VarPos(_, _) => todo!(),
    //         Parameter::VarKeys(_, _) => todo!(),
    //     }
    // }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Arg<'src> {
    Pos(Span, Expr<'src>),               // foo(1)
    ByName(Span, &'src str, Expr<'src>), // foo(a: 1)
    ExpandPos(Span, Expr<'src>),         // ..[a, b, c]
    ExpandKeys(Span, Expr<'src>),        // ...{a: 1, b: 2}
}

impl<'src> Arg<'src> {
    pub fn pretty<'a, D, A>(&'a self, allocator: &'a D) -> DocBuilder<'a, D, A>
    where
        D: DocAllocator<'a, A>,
        D::Doc: Clone,
        A: Clone,
    {
        match &*self {
            Arg::Pos(_, arg) => 
                allocator.text("arg-positional ").append(arg.pretty(allocator)),
            Arg::ByName(_, name, arg) => 
                allocator.text("arg-by-name ").append(allocator.text(*name)).append(arg.pretty(allocator)),
            Arg::ExpandPos(_, arg) => 
                allocator.text("arg-expand-pos").append(arg.pretty(allocator)),
            Arg::ExpandKeys(_, arg) =>
                allocator.text("arg-expand-keys").append(arg.pretty(allocator)),
        }.parens().group()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Expr<'src> {
    Identifier(Span, &'src str),
    Literal(Span, Literal),
    List(Span, Vec<Expr<'src>>),              // list literal [a; b; c; d]
    Tuple(Span, Vec<Expr<'src>>),             // tuple literal (1, 2, 3)
    Record(Span, Vec<Field<'src>>),           // record literal { a = 1, b = 2 }
    Prefix(Span, &'src str, Box<Expr<'src>>), // -1
    Infix(Span, Vec<Expr<'src>>, Vec<&'src str>), // 1 - 1
    Call(Span, Box<Expr<'src>>, Vec<Arg<'src>>), // a(b, c)

    Scope(Span, Declarations<'src>, Box<Option<Expr<'src>>>), // { a }, does not allow public
    Lambda(Span, Vec<Parameter<'src>>, Box<Expr<'src>>),      // Rust-like: |a, b| a
    // if a == 1 { x } else { y }, must have braces, else is optional
    IfElse(
        Span,
        Box<Expr<'src>>,
        Box<Expr<'src>>,
        Option<Box<Expr<'src>>>,
    ),
    Project(Span, Box<Expr<'src>>, &'src str), // foo.bar or foo::bar, both are equivalent
    Match(Span, Box<Expr<'src>>, Vec<(Pattern<'src>, Expr<'src>)>),
    Module(Declarations<'src>), // mod { pub let a = 1, let b = 2, etc}, allows public
    Builtin(Span, &'src str, Vec<Expr<'src>>)
}

impl<'src> Expr<'src> {
    pub fn pretty<'a, D, A>(&'a self, _allocator: &'a D) -> DocBuilder<'a, D, A>
    where
        D: DocAllocator<'a, A>,
        D::Doc: Clone,
        A: Clone,
    {
        todo!()
    }
}


// a bunch of declarations that are all anded together
// i.e the bindings are mutually recursive
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Declarations<'src> {
    pub span: Span,
    pub declarations: Vec<Declaration<'src>>,
}


#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ReplInput<'src> {
    Decl(Declaration<'src>),
    Expr(Expr<'src>),
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

pub fn transpile_infix(
    args: &Vec<Expr<'_>>,
    ops: &Vec<&str>,
    env: &SymbolMap,
    _span: Option<Span>,
    builder: ExprBuilder<'_>
) {
    if args.len() == 1 && ops.len() == 0 {
        // just transpile as per normal
        args[0].transpile(env, builder);
        return;
    }
    if args.len() < 2 {
        let mut eb = builder.init_error();
        eb.set_summary("Must provide at least two arguments to infix expression");
        return;
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

    if let Some(sym) = env.lookup(op) {
        // Return a call expression
        let ib = builder.init_invoke();
        let mut cb = ib.init_app();
        let lx = cb.reborrow().init_lam();
        let mut lx = lx.init_id();
        lx.set_name(op);
        lx.set_disam(sym);

        let mut args = cb.reborrow().init_args(2);
        // get the builder for the args
        // and transpile left and right arguments
        let mut lb = args.reborrow().get(0);
        lb.set_pos(());
        transpile_infix(&largs, &lops, env, None,  lb.init_value());
        let mut rb = args.reborrow().get(1);
        rb.set_pos(());
        transpile_infix(&rargs, &rops, env, None,  rb.init_value());
    } else {
        let mut eb = builder.init_error();
        eb.set_summary("Symbol not found");
    }
}

impl<'src> Expr<'src> {
    pub fn transpile(&self, env: &SymbolMap, builder: ExprBuilder<'_>) {        
        match self {
            Expr::Identifier(_, ident) => {
                match env.lookup(ident) {
                    None => {
                        let mut eb = builder.init_error();
                        eb.set_summary("Unrecognized symbol");
                    },
                    Some(disam) => {
                        let mut sb = builder.init_id();
                        sb.set_name(ident);
                        sb.set_disam(disam);
                    }
                }
            },
            Expr::Infix(s, args, ops) => transpile_infix(args, ops, env, Some(*s), builder),
            Expr::Literal(_, lit) => lit.transpile(builder.init_literal()),
            Expr::IfElse(_, scrutinized, if_branch, else_branch) => 
                {
                    // yo dawg i heard you like reborrows so i reborrowed your reborrow
                    // so when you reborrow its already borrowed
                    let mut mb = builder.init_match();
                    scrutinized.transpile(env, mb.reborrow().init_expr());
                    let mut cb = mb.reborrow().init_cases(2);
                    let mut true_case = cb.reborrow().get(0);
                    true_case.reborrow().init_eq().set_bool(true);
                    if_branch.transpile(env, true_case.init_expr());
                    let mut false_case = cb.reborrow().get(1);
                    false_case.reborrow().init_eq().set_bool(false);
                    
                    if let Some(else_expr) = else_branch {
                        else_expr.transpile(env, false_case.init_expr())
                    } else {
                        // if else is omitted then default eval to unit makes sense?
                        false_case.init_expr().init_literal().set_unit(());
                    }
                    
                    let mut bb = mb.reborrow().init_binding();
                    bb.set_omitted(());
                },
            Expr::Tuple(_, items) => todo!(),
            Expr::Builtin(_, name, args) => {
                let mut bb = builder.init_inline_builtin();
                bb.set_op(name);
                let mut ba = bb.reborrow().init_args(args.len() as u32);
                for (i,arg) in args.iter().enumerate() {
                    arg.transpile(env, ba.reborrow().get(i as u32));
                };
            },
            Expr::Record(s, fields) => {
                if let Some((hd, tl)) = fields.split_first() {
                    match hd {
                        Field::Simple(_, field_name, val) => {
                            let mut bb =  builder.init_inline_builtin();
                            bb.set_op("__insert");
                            let mut args_builder = bb.init_args(3);
                            Expr::Record(*s, tl.to_vec()).transpile(env, args_builder.reborrow().get(0));
                            args_builder.reborrow().get(1).init_literal().set_string(field_name);
                            val.transpile(env, args_builder.reborrow().get(2));
                        },
                        _ => todo!() // need to unpacking of record expansions -\_(o o)_/-
                    }
                } else {
                    builder.init_literal().set_empty_record(());
                }
            }
            Expr::Module(decls) => {
                let mut let_builder = builder.init_let();
                let mut child_env = SymbolMap::child(&env);
                decls.transpile(&mut child_env, let_builder.reborrow().init_binds());
                
                // collect the public decls as names in our record
                let pub_names = decls.declarations.iter().filter_map(extract_name).collect::<Vec<&str>>();

                let placeholder_span = Span::new(0, 0);
                let fields = 
                    pub_names.iter().map(|n| {Field::Simple(placeholder_span, n, Expr::Identifier(placeholder_span, n))});

                let record = Expr::Record(placeholder_span, fields.collect());
                record.transpile(&child_env, let_builder.reborrow().init_body())
            },
            Expr::Lambda(_, params, body) => {
                let mut lb = builder.init_lam();
                let mut pb = lb.reborrow().init_params(params.len() as u32);
                
                let mut new_env = SymbolMap::child(env);

                // doing param transpilation here for now since
                // it makes generating Symbol Maps easier
                for (i,param) in params.iter().enumerate() {
                    match param {
                        Parameter::Named(_, name) => {
                            let mut sym_builder = pb.reborrow().get(i as u32).init_symbol();
                            let disam = new_env.add(name);
                            sym_builder.set_disam(disam);
                            sym_builder.set_name(name);
                            pb.reborrow().get(i as u32).set_pos(());
                        },
                        _ => todo!(),
                    }
                };
                body.transpile(&new_env, lb.reborrow().init_body());
            },
            Expr::List(s, items) => {
                if let Some((hd, tl)) = items.split_first() {
                    let mut bb = builder.init_inline_builtin();
                    bb.set_op("__cons");
                    let mut args_builder = bb.reborrow().init_args(2);
                    let head = args_builder.reborrow().get(0);
                    hd.transpile(env, head);
                    let cons = args_builder.reborrow().get(1);
                    // i think this is O(n^2) ?
                    // could prob fix with slices or less recursion
                    Expr::List(*s, tl.to_vec()).transpile(env,cons);
                } else {
                    builder.init_literal().set_empty_list(());
                }
            }
            Expr::Prefix(_, _, _) => todo!(),
            // a call is an application of args followed by an invoke
            Expr::Call(_, fun, args) => {
                let ib = builder.init_invoke();
                let mut app_b = ib.init_app();
                fun.transpile(env, app_b.reborrow().init_lam());
                let mut args_b = app_b.reborrow().init_args(args.len() as u32);
                for (i, arg) in args.iter().enumerate() {
                    match arg {
                        Arg::Pos(_, arg_val) => {
                                args_b.reborrow().get(i as u32).set_pos(());
                                arg_val.transpile(env, args_b.reborrow().get(i as u32).init_value());
                            }
                        _ => todo!(),
                    }
                };
            },
            Expr::Scope(_, decls, val) => {
                let mut let_builder = builder.init_let();
                let mut child_env = SymbolMap::child(&env);
                decls.transpile(&mut child_env, let_builder.reborrow().init_binds());
                if let Some(e) = val.deref() {
                    e.transpile(&child_env, let_builder.reborrow().init_body())
                } else {
                    let_builder.reborrow().init_body().init_literal().set_unit(());
                }
            },
            Expr::Project(_, _, _) => todo!(),
            Expr::Match(_, _, _) => todo!(),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct LetBinding<'src> {
    pub binding: (Pattern<'src>, Expr<'src>),
}

impl<'src> LetBinding<'src> {
    pub fn new(b: (Pattern<'src>, Expr<'src>)) -> Self {
        LetBinding { binding: b }
    }

    // TODO: mutually recursive binds are not detected!!!
    pub fn transpile(&self, env: &mut SymbolMap, mut builder: BindBuilder<'_>) {
        match self {
            LetBinding{binding:(Pattern::Identifier(_, name), e)}=> {
                e.transpile(&env,builder.reborrow().init_value());
                let disam = env.add(name);
                let mut sym_builder = builder.reborrow().init_symbol();
                sym_builder.set_disam(disam);
                sym_builder.set_name(name);
            },
            _ => todo!()
        }
    }

    /*
    pub fn transpile<'a>(&self, env: &'a SymbolEnv) -> (CoreBind, SymbolEnv<'a>) {
        let mut nenv = SymbolEnv::child(env);
        let new_symbols = self.create_symbols(&mut nenv);
        let mut bindings : Vec<(Symbol, CoreExpr)> = Vec::new();
        for (i, (binding, mut symbols)) in self.bindings.iter().zip(new_symbols.into_iter()).enumerate() {
            match binding {
                LetBinding::Pattern(_, pat, val) => {
                    let val_expr = val.transpile(&nenv);
                    if symbols.len() == 1 {
                        bindings.push((symbols.pop().unwrap(), pat.deconstruct(0, val_expr)))
                    } else {
                        // unse an intermediate id to then destructure
                        // you cannot normally create an id starting with #
                        // so this is fine
                        let val_id = env.next_id(format!("#{}", i));
                        // deconstruct each of the symbols individually
                        for (si, s) in symbols.into_iter().enumerate() {
                            bindings.push((s,
                                pat.deconstruct(si, CoreExpr::Atom(Atom::Id(val_id.clone())))));
                        }
                        // put the value binding in
                        bindings.push((val_id, val_expr));
                    }
                },
                LetBinding::Function(_, name, _, args, body) => {
                    // for this we need to create the internal argument bindings
                    let mut internal_env = SymbolEnv::child(&nenv);
                    let internal_symbols : Vec<Vec<Symbol>> = args.iter()
                                    .map(|x| x.create_symbols(&mut internal_env))
                                    .collect();
                    // transpile the body
                    let mut func = body.transpile(&internal_env);
                    for (_ai, (arg, mut syms)) in args.iter().rev().zip(
                                    internal_symbols.into_iter().rev()).enumerate() {
                        if let Parameter::Pattern(pat) = arg {
                            match pat {
                                Pattern::Identifier(_, _) => {
                                    func = CoreExpr::Lam(
                                        syms.pop().unwrap(),
                                        func.as_body()
                                    );
                                },
                                _pattern => {
                                    panic!("Cannot handle arbitrary patterns rn")
                                }
                            }
                        } else {
                            panic!("Can only handle positional parameters for now");
                        }
                    }
                    // add the external function binding
                    bindings.push((env.next_id(name.to_string()), func));
                },
                LetBinding::Error(_) => panic!("Erorr in bindings")
            }
        }
        if bindings.len() == 1 {
            let b = bindings.pop().unwrap();
            (CoreBind::NonRec(b.0, Box::new(b.1)), nenv)
        } else {
            (CoreBind::Rec(bindings), nenv)
        }
    }
    */
}

// A declaration is a top-level
// type statement/let statement/export statement
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Declaration<'src> {
    LetDeclare(Span, bool, LetBinding<'src>),
    FnDeclare(Span, bool, &'src str, Vec<Parameter<'src>>, Expr<'src>, Option<Vec<&'src str>>), // optional list of annotations
}


impl<'src> Declaration<'src> {
    pub fn transpile<'a>(&self, env: &'a mut SymbolMap, mut builder: BindBuilder) {
        match self {
            Self::LetDeclare(_, _exported, binding) => {
                binding.transpile(env, builder);
            },
            // TODO: annotations?
            Declaration::FnDeclare(s, b, name, params, body, _) => {
                let disam = env.add(name);
                let mut sym_builder = builder.reborrow().init_symbol();
                sym_builder.set_disam(disam);
                sym_builder.set_name(name);
                Expr::Lambda(*s, params.clone(), Box::new(body.clone())).transpile(&env, builder.init_value());
            },
        }
    }

    pub fn set_public(&mut self, is_public: bool) {
        let b = match self {
            Declaration::LetDeclare(_, b, _) => b,
            Declaration::FnDeclare(_, b, _, _, _, _) => b,
        };
        *b = is_public;
    }
}

fn extract_name<'a>(d: &'a Declaration) -> Option<&'a str> {
    match d {
        Declaration::LetDeclare(_, true, LetBinding{binding:(Pattern::Identifier(_, name),_)}) => {
            Some(name)
        },
        Declaration::FnDeclare(_, true, name, _, _, _) => Some(name),
        _ => None
    }
}

impl<'src> Declarations<'src> {
    pub fn new(span: Span, declarations: Vec<Declaration<'src>>) -> Self {
        Declarations { span, declarations }
    }

    pub fn transpile<'a>(&self, env: &'a mut SymbolMap, builder: BindsBuilder) {
        let mut bb = builder.init_binds(self.declarations.len() as u32);
        for (i,decl) in self.declarations.iter().enumerate() {
            decl.transpile(env,bb.reborrow().get(i as u32));
        }
    }
}
