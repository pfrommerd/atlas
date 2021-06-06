use ordered_float::NotNan;
use std::collections::HashMap;

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
    Symbol, SymbolEnv, Atom,
    Expr as CoreExpr,
    Format,
    Literal as CoreLiteral,
    Bind as CoreBind
};

// Type patterns are not like expression patterns!
// Type patterns match against types at compile time and are not lazily evaluated
// Only types can go in expression patterns i.e (A, int) = (float, int) the left hand pattern
// will match A to float

// Expression patterns are evaluated at run time and are for non-types i.e (0, x) = (0, 1) matches x = 1

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum FieldType<'src> {
    Default(Span, &'src str), // {Bar} (equivalent to {Bar: Bar})
    Simple(Span, &'src str, Span, Type<'src>), // a : int
    Expansion(Span, Type<'src>) // ...another_type
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ArgType<'src> { // a tuple entry
    Positional(Type<'src>),              // int
    Named(Span, &'src str, Span, Type<'src>),       // ~(foo:int)
    VariablePositional(Span, Type<'src>),          // ..int list

    Optional(Span, &'src str, Span, Type<'src>),    // ?(foo:int)
    VariableOptional(Span, Type<'src>),            // ...int dict
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Type<'src> {
    Hole(Span),                                            // cannot end up in a concrete type!
    Identifier(Span, &'src str),                           // A type identifier
    Applied(Span, Vec<Type<'src>>, Box<Type<'src>>),         // int int tree or even 'a tree, 

    Project(Span, Box<Type<'src>>, &'src str),              // type.field

    Arrow(Span, Vec<ArgType<'src>>, Box<Type<'src>>),            // 'a -> 'b -> 'c

    Variant(Span, Vec<(&'src str, Type<'src>)>),      // A int | B (float,float) | C
    Tuple(Span, Vec<Type<'src>>),                     // (int, float, string) 
    Record(Span, Vec<FieldType<'src>>),               // { a : int, b : float, ..another type }

    // shorthands like [int] instead of int list
    // List(Span, Box<Type<'src>>)

    Error()
}

// A type binding is a type (used like a pattern) on the lhs
// and a type on the rhs
// Note that this can result in multiple types potentially
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TypeBindings<'src> {
    pub bindings: Vec<(Type<'src>, Type<'src>)>
}

impl<'src> TypeBindings<'src> {
    pub fn new(b: Vec<(Type<'src>, Type<'src>)>) -> Self {
        TypeBindings { bindings: b }
    }
}

// Expression-related structs

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Literal {
    Unit,
    Bool(bool),
    Int(i64),
    Float(NotNan<f64>),
    String(String),
    Char(char)
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

// Fields that come later override fields that come earlier
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum FieldExpr<'src> {
    Default(Span, &'src str),
    Simple(Span, &'src str, Span, Expr<'src>), // let c = { a : 0, b : 1}
    Expansion(Span, Expr<'src>) // let c = { a : 0, ...b }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Expr<'src> {
    Identifier(Span, &'src str),
    Literal(Span, Literal),
    Constraint(Box<Expr<'src>>, Type<'src>), // type constraint

    List(Span, Vec<Expr<'src>>), // list literal [a; b; c; d]
    Record(Span, Vec<FieldExpr<'src>>), // record literal { a = 1, b = 2 }

    Prefix(Span, &'src str, Box<Expr<'src>>),   // any operator that starts with a !
                                                // like !$foo will be Unary(!$, foo)
    Infix(Span, Vec<Expr<'src>>, Vec<&'src str>), 
                                                // 1 + 2 * 3 will be turned into Infix([(1 +), (2, *)], 3) and
                                                // operator precedent/associativity will be
                                                // determined in the compilation stage
    App(Span, Box<Expr<'src>>, Vec<Expr<'src>>),

    Macro(Span, &'src str, Vec<Expr<'src>>), // string! expr1 expr2 will be evaluated at module
                                             // instantiation time and can add dependencies to
                                             // the module expression

    // scoped let/type declarations
    LetIn(Span, LetBindings<'src>, Box<Expr<'src>>), 
    TypeIn(Span, TypeBindings<'src>, Box<Expr<'src>>),

    IfElse(Span, Box<Expr<'src>>, Box<Expr<'src>>, Box<Expr<'src>>),

    Project(Span, Box<Expr<'src>>, &'src str),

    // match with syntax, note that the tuples are not 
    Match(Span, Box<Expr<'src>>, Vec<(Pattern<'src>, Expr<'src>)>), 
    Fun(Span, Vec<Parameter<'src>>, Box<Expr<'src>>),
    Module(Module<'src>)
}

fn symbol_priority(sym: &str) -> u8 {
    match sym {
        "-" => 0,
        "+" => 0,
        "*" => 1,
        "/" => 1,
        _ => 2
    }
}

impl<'src> Expr<'src> {
    pub fn transpile(&self, env: &SymbolEnv) -> CoreExpr {
        match self {
            Expr::Literal(_, lit) => CoreExpr::Atom(Atom::Lit(lit.to_core())),
            Expr::Identifier(_, ident) => {
                if let Some(sym) = env.lookup(ident) {
                    CoreExpr::Atom(Atom::Id(sym.clone()))
                } else {
                    CoreExpr::Bad
                }
            },
            Expr::Infix(span, args, ops) => {
                Expr::transpile_infix(args, ops, env, Some(*span))
            },
            Expr::App(_, func, args) => {
                let func_comp = func.transpile(env);
                let args_comp = args.iter().map(|x| x.transpile(env)).collect();
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
                return CoreExpr::Let(bind, Atom::new(body_expr));
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
                            args.push(CoreExpr::Atom(Atom::Id(symb)));
                            if exp {
                                Some(name)
                            } else {
                                None
                            }
                        }).collect());
                let mut declr = CoreExpr::apply(CoreExpr::Pack(0, args.len(), f), args);
                for b in bindings.into_iter().rev() {
                    declr = CoreExpr::Let(b, Atom::new(declr));
                }
                declr
            },
            _ => panic!("Unrecognized transpilation type!")
        }
    }

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
                    Atom::new(CoreExpr::App(
                    Atom::Id(sym.clone()), 
                    Atom::new(Expr::transpile_infix(&largs, &lops, env, None)),
                )), Atom::new(Expr::transpile_infix(&rargs, &rops, env, None)))
            } else {
                CoreExpr::Bad
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Pattern<'src> {
    Hole(Span), // _
    Identifier(Span, &'src str)
}

impl<'src> Pattern<'src> {
    pub fn create_symbols(&self, env: &mut SymbolEnv) -> Vec<Symbol> {
        match self {
            Pattern::Identifier(_, s) => {
                let id = env.next_id(s.to_string());
                env.add(id.clone());
                vec![id]
            },
            Pattern::Hole(_) => Vec::new()
        }
    }

    pub fn num_symbols(&self) -> usize {
        match self {
            Pattern::Identifier(_, _) => 1,
            Pattern::Hole(_) => 0
        }
    }

    pub fn deconstruct(&self, idx: usize, expr : CoreExpr) -> CoreExpr {
        if idx > self.num_symbols() {
            panic!("No such symbol to deconstruct")
        }
        match self {
            Pattern::Identifier(_, _) => expr,
            _ => panic!("Unable to deconstruct")
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Parameter<'src> {
    Pattern(Pattern<'src>),
    VariablePositional(Span, &'src str, Span, Option<Type<'src>>, Option<Pattern<'src>>),
    Named(Span, &'src str, Span, Option<Type<'src>>, Option<Pattern<'src>>),
    Optional(Span, &'src str),
    VariableOptional(Span, &'src str, Option<Type<'src>>)
}

impl<'src> Parameter<'src> {
    pub fn create_symbols(&self, env: &mut SymbolEnv) -> Vec<Symbol> {
        match self {
            Parameter::Pattern(pat) => pat.create_symbols(env),
            _ => panic!("Non-pattern parameters not implemented")
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum LetBinding<'src> {
    Pattern(Span, Pattern<'src>, Expr<'src>),
    Function(Span, &'src str, Span, Vec<Parameter<'src>>, Expr<'src>),
    Error(Span)
}

impl<'src> LetBinding<'src> {
    pub fn create_symbols(&self, env: &mut SymbolEnv<'_>) -> Vec<Symbol> {
        match self {
            LetBinding::Pattern(_, pat, _) => pat.create_symbols(env),
            LetBinding::Function(_, f, _, _, _) => {
                let id = env.next_id(f.to_string());
                env.add(id.clone());
                vec![id]
            },
            LetBinding::Error(_) => Vec::new()
        }
    }

    pub fn num_symbols(&self) -> usize {
        match self {
            LetBinding::Pattern(_, pat, _) => pat.num_symbols(),
            LetBinding::Function(_, _, _, _, _) => 1,
            LetBinding::Error(_) => 0
        }
    }
}

// various and'ed bindings
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct LetBindings<'src> {
    // anded bindings
    pub bindings: Vec<LetBinding<'src>>
}

// various and'ed bindings
impl<'src> LetBindings<'src> {
    pub fn new(b : Vec<LetBinding<'src>>) -> Self {
        LetBindings{ bindings: b }
    }

    pub fn create_symbols(&self, env: &mut SymbolEnv<'_>) -> Vec<Vec<Symbol>> {
        let mut symbols = Vec::new();
        for x in self.bindings.iter() {
            symbols.push(x.create_symbols(env));
        }
        symbols
    }

    pub fn num_symbols(&self) -> Vec<usize> {
        self.bindings.iter().map(|x| x.num_symbols()).collect()
    }

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
                                        Atom::new(func)
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
}

// A declaration is a top-level 
// type statement/let statement/export statement
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Declaration<'src> {
    // bool is whether this declaration is exported
    TypeDeclare(Span, bool, TypeBindings<'src>),

    // bool is whether this declaration is exported
    LetDeclare(Span, bool, LetBindings<'src>), 

    MacroDeclare(Span, bool, Expr<'src>)
}

impl<'src> Declaration<'src> {
    pub fn transpile<'a>(&self, env: &'a SymbolEnv) -> (Vec<CoreBind>, SymbolEnv<'a>) {
        match self {
            Self::LetDeclare(_, _exported, bindings) => {
                let (bind, nenv) = bindings.transpile(env);
                (vec![bind], nenv)
            }, 
            Self::TypeDeclare(_, _exported, _bindings) => panic!("Type not yet implemented"),
            Self::MacroDeclare(_, _, _) => panic!("Macro not yet implemented")
        }

    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ReplInput<'src> {
    Decl(Declaration<'src>),
    Expr(Expr<'src>),
    Type(Type<'src>)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Module<'src> {
    pub span : Span,
    pub declarations: Vec<Declaration<'src>>
}

impl<'src> Module<'src> {
    pub fn new(span: Span, declarations: Vec<Declaration<'src>>) -> Self {
        Module{span, declarations}
    }
}