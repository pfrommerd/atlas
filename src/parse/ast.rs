use ordered_float::NotNan;

pub use codespan::{
    ByteIndex,
    ColumnIndex,
    LineIndex,
    ColumnOffset,
    LineOffset,
    ByteOffset,
    Span
};
use crate::core::lang::{
    SymbolEnv, Atom,
    Expr as CoreExpr,
    Literal as CoreLiteral,
    PrimitiveType
};

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Literal {
    Unit,
    Bool(bool),
    Int(i64),
    Float(NotNan<f64>),
    String(String),
    Char(char)
}


// Fields that come later override fields that come earlier
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Field<'src> {
    Shorthand(Span, &'src str),
    Simple(Span, &'src str, Expr<'src>), // a : 0
    Expansion(Span, Expr<'src>) // ...b
}

// Patterns

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum FieldPattern<'src> {
    Shorthand(Span, &'src str), // as in {a, b, c},
    Simple(Span, &'src str, Pattern<'src>), // as in {a: (a, b)}, will bind a, b
    Expansion(Span, Option<&'src str>) // {...bar} or {a, ...}
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Pattern<'src> {
    Hole(Span), // _
    Identifier(Span, &'src str),
    Literal(Span, CoreLiteral),
    Tuple(Span, Vec<Pattern<'src>>),
    Record(Span, Vec<FieldPattern<'src>>),
    Var(Span, &'src str, Option<Box<Pattern<'src>>>),
    Of(Span, PrimitiveType, &'src str) // int(a), float(b), etc. allows matching by type
}

// Argument types

// Parameter is for the declaration, arg is for the call
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Parameter<'src> {
    Named(Span, &'src str), // fn foo(a)
    Pattern(Span, Pattern<'src>),
    NamedPattern(Span, &'src str, Pattern<'src>),
    VarPos(Span, Option<&'src str>), // fn foo(..a)
    Optional(Span, &'src str),
    VarKeys(Span, Option<&'src str>) // fn foo(...a)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Arg<'src> {
    Record(Span, Vec<Field<'src>>),
    Pos(Span, Expr<'src>), // foo(1) 
    ByName(Span, &'src str, Expr<'src>), // foo(a: 1)
    ExpandPos(Span, Expr<'src>), // ..[a, b, c]
    ExpandKeys(Span, Expr<'src>), // ...{a: 1, b: 2}
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Expr<'src> {
    Identifier(Span, &'src str),
    Literal(Span, Literal),
    List(Span, Vec<Expr<'src>>), // list literal [a; b; c; d]
    Tuple(Span, Vec<Expr<'src>>), // tuple literal (1, 2, 3)
    Record(Span, Vec<Field<'src>>), // record literal { a = 1, b = 2 }
    Prefix(Span, &'src str, Box<Expr<'src>>), // -1
    Infix(Span, Vec<Expr<'src>>, Vec<&'src str>), // 1 - 1

    App(Span, Box<Expr<'src>>, Vec<Arg<'src>>), // a @ (b, c)
    Call(Span, Box<Expr<'src>>, Vec<Arg<'src>>), // a(b, c)

    Scope(Span, Declarations<'src>, Box<Expr<'src>>), // { a }, does not allow public
    Lambda(Span, Vec<Parameter<'src>>, Box<Expr<'src>>), // Rust-like: |a, b| a
    // if a == 1 { x } else { y }, must have braces, else is optional
    IfElse(Span, Box<Expr<'src>>, Box<Expr<'src>>, Option<Box<Expr<'src>>>), 
    Project(Span, Box<Expr<'src>>, &'src str), // foo.bar or foo::bar, both are equivalent
    Match(Span, Box<Expr<'src>>, Vec<(Pattern<'src>, Expr<'src>)>), 
    Module(Declarations<'src>) // mod { pub let a = 1, let b = 2, etc}, allows public
}

// a bunch of declarations that are all anded together
// i.e the bindings are mutually recursive
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Declarations<'src> {
    pub span : Span,
    pub declarations: Vec<Declaration<'src>>
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ReplInput<'src> {
    Decl(Declaration<'src>),
    Expr(Expr<'src>)
}

impl Literal {
    fn to_core(&self) -> CoreLiteral {
        match self {
            Self::Unit => CoreLiteral::Unit,
            Self::Bool(b) => CoreLiteral::Bool(*b),
            Self::Int(i) => CoreLiteral::Int(*i),
            Self::Float(f) => CoreLiteral::Float(*f),
            Self::String(s) => CoreLiteral::String(s.clone()),
            Self::Char(c) => CoreLiteral::Char(*c)
        }
    }
}


/*
fn symbol_priority(sym: &str) -> u8 {
    match sym {
        "-" => 0,
        "+" => 0,
        "*" => 1,
        "/" => 1,
        _ => 2
    }
}
*/

impl<'src> Expr<'src> {
    pub fn transpile(&self, env: &SymbolEnv) -> CoreExpr {
        match self {
            Expr::Identifier(_, ident) => {
                if let Some(sym) = env.lookup(ident) {
                    CoreExpr::Atom(Atom::Id(sym.clone()))
                } else {
                    CoreExpr::Bad
                }
            },
            Expr::Literal(_, lit) => 
                CoreExpr::Atom(Atom::Lit(lit.to_core())),
            Expr::List(_, _) => {
                panic!("Unable to handle list literals")
            },
            /*
            Expr::Tuple(_, val) => {
                let pack = Atom::Pack(0, val.len(), Format::Tuple(val.len())).as_body();
                CoreExpr::apply(pack, 
                val.iter().map(|x| x.transpile(env).as_body()).collect())
            },
            Expr::Record(_, fields) => {
                let mut args = Vec::new();
                let pack = Atom::Pack(0, fields.len(), 
                    Format::Fields(fields.iter().map(|f| {
                        match f {
                            FieldExpr::Default(_, n) => {
                                args.push(Atom::Id(env.lookup(n).unwrap().clone()).as_body());
                                n.to_string()
                            },
                            FieldExpr::Simple(_, n, _, val) => {
                                args.push(val.transpile(env).as_body());
                                n.to_string()
                            },
                            FieldExpr::Expansion(_, _) => panic!("unable to handle expansions")
                        }
                    }).collect())).as_body();
                CoreExpr::apply(pack, args)
            },
            Expr::Infix(span, args, ops) => {
                Expr::transpile_infix(args, ops, env, Some(*span))
            },
            Expr::App(_, func, args) => {
                let func_comp = func.transpile(env).as_body();
                let args_comp = args.iter().map(|x| x.transpile(env).as_body()).collect();
                CoreExpr::apply(func_comp, args_comp)
            },
            Expr::LetIn(_, bindings, body) => {
                let (bind, nenv) = bindings.transpile(env);
                let body_expr = body.transpile(&nenv);
                if let CoreBind::Rec(binds) = &bind {
                    if binds.is_empty() {
                        return body_expr;
                    }
                }
                return CoreExpr::Let(bind, body_expr.as_body());
            },
            Expr::Module(Module{span: _, declarations}) => {
                let mut vars : HashMap<String, (bool, Symbol)> = HashMap::new();
                let mut bindings = Vec::new();
                let mut nenv = SymbolEnv::child(env);
                for d in declarations {
                    match d {
                        Declaration::LetDeclare(_, exported, binds) => {
                            let (b, ne) = binds.transpile(&nenv);
                            bindings.push(b);
                            // add the child's bindings
                            for (name, s) in ne.symbols.iter() {
                                vars.insert(name.clone(), (*exported, s.clone()));
                            }
                            let syms : HashMap<String, Symbol> = ne.symbols;
                            nenv.extend(syms);
                        },
                        _ => panic!("Unimplemented declaration")
                    }
                }
                // construct the type, pack
                let mut args = Vec::new();
                let f = Format::Fields(vars.into_iter()
                        .filter_map(|(name, (exp, symb))| {
                            args.push(Atom::Id(symb).as_body());
                            if exp {
                                Some(name)
                            } else {
                                None
                            }
                        }).collect());
                let mut declr = CoreExpr::apply(
                    Atom::Pack(0, args.len(), f).as_body(), 
                    args);
                for b in bindings.into_iter().rev() {
                    declr = CoreExpr::Let(b, declr.as_body());
                }
                declr
            },
            Expr::Project(_, body, name) => {
                let b = CoreExpr::App(
                    Atom::As(Format::Fields(vec![String::from(*name)])).as_body(),
                        body.transpile(env).as_body());
                CoreExpr::App(
                        CoreExpr::App(
                            b.as_body(),
                            Atom::Lit(CoreLiteral::Int(0)).as_body()
                        ).as_body(),
                        body.transpile(env).as_body()
                    )
            },
            */
            _ => panic!("Unrecognized transpilation type!")
        }
    }

    /*
    pub fn transpile_infix(args : &Vec<Expr<'src>>, 
                    ops : &Vec<&str>, env : &SymbolEnv, _span: Option<Span>) -> CoreExpr {
        if args.is_empty() {
            return CoreExpr::Bad;
        }
        if args.len() == 1 {
            args.first().unwrap().transpile(env)
        } else {
            let mut lowest_priority : u8 = 255;
            let mut left_assoc = false;
            let mut split_idx = 0;
            for (idx, op) in ops.iter().enumerate() {
                if let Some(sym) = env.lookup(op) {
                    let p = symbol_priority(sym.name.as_str());
                    if p < lowest_priority {
                        lowest_priority = p;
                        left_assoc = true;
                        split_idx = idx;
                    }
                    if lowest_priority == p && left_assoc {
                        split_idx = idx;
                    }
                }
            }
            let mut largs = args.clone();
            let rargs = largs.split_off(split_idx + 1);

            let mut lops = ops.clone();
            let mut cops = lops.split_off(split_idx);
            let rops = cops.split_off(1);
            let op = cops[0];
            if let Some(sym) = env.lookup(op) {
                CoreExpr::App(
                    CoreExpr::App(
                        Atom::Id(sym.clone()).as_body(), 
                        Expr::transpile_infix(&largs, &lops, env, None).as_body()
                    ).as_body(),
                    Expr::transpile_infix(&rargs, &rops, env, None).as_body())
            } else {
                CoreExpr::Bad
            }
        }
    }
    */
}


// various and'ed bindings
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct LetBindings<'src> {
    // anded bindings
    pub bindings: Vec<(Pattern<'src>, Expr<'src>)>,
}
// various and'ed bindings
impl<'src> LetBindings<'src> {
    pub fn new(b : Vec<(Pattern<'src>, Expr<'src>)>) -> Self {
        LetBindings{ bindings: b }
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
    LetDeclare(Span, bool, LetBindings<'src>), 
    FnDeclare(Span, bool, &'src str, Expr<'src>),
    MacroDeclare(Span, bool, Expr<'src>)
}

impl<'src> Declaration<'src> {
    /*pub fn transpile<'a>(&self, env: &'a SymbolEnv) -> (Vec<CoreBind>, SymbolEnv<'a>) {
        match self {
            Self::LetDeclare(_, _exported, bindings) => {
                let (bind, nenv) = bindings.transpile(env);
                (vec![bind], nenv)
            }, 
            Self::MacroDeclare(_, _, _) => panic!("Macro not yet implemented")
        }
    }*/
    pub fn set_public(&mut self, is_public: bool) {
        let b = match self {
            Declaration::LetDeclare(_, b, _) => b,
            Declaration::MacroDeclare(_, b, _) => b,
            Declaration::FnDeclare(_, b, _, _) => b
        };
        *b = is_public;
    }
}

impl<'src> Declarations<'src> {
    pub fn new(span: Span, declarations: Vec<Declaration<'src>>) -> Self {
        Declarations{span, declarations}
    }
}