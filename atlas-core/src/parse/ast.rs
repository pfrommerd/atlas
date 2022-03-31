use ordered_float::NotNan;

pub use codespan::{ByteIndex, ByteOffset, ColumnIndex, ColumnOffset, LineIndex, LineOffset, Span};

pub const PRELUDE: &'static str = include_str!("../../prelude/prelude.at");

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Literal {
    Unit,
    Bool(bool),
    Int(i64),
    Float(NotNan<f64>),
    String(String),
    Char(char),
}

// Fields that come later override fields that come earlier
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Field<'src> {
    Shorthand(Span, &'src str),
    Simple(Span, &'src str, Expr<'src>), // a : 0
    Expansion(Span, Expr<'src>),         // ***b
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum FieldPattern<'src> {
    Shorthand(Span, &'src str),             // as in {a, b, c},
    Simple(Span, &'src str, Pattern<'src>), // as in {a: (a, b)}, will bind a, b
    Expansion(Span, Option<&'src str>),     // {...bar} or {a, ...}
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Item<'src> {
    Simple(Span, Expr<'src>),
    Expansion(Span, Option<&'src str>)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ItemPattern<'src> {
    Simple(Span, Pattern<'src>),
    Expansion(Span, Option<&'src str>)
}


#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Pattern<'src> {
    Hole(Span), // _
    Identifier(Span, &'src str),
    Literal(Span, Literal),
    Tuple(Span, Vec<Pattern<'src>>),
    List(Span, Vec<ItemPattern<'src>>), 
    Record(Span, Vec<FieldPattern<'src>>),
    TupleVariant(Span, &'src str, Vec<Pattern<'src>>),
    RecordVariant(Span, &'src str, Vec<FieldPattern<'src>>)
}

// Parameter is for the declaration, arg is for the call
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Parameter<'src> {
    Named(Span, &'src str), // fn foo(a)
    // Optional(Span, &'src str), // fn foo(?a)
    // VarPos(Span, Option<&'src str>), // fn foo(..a)
    // VarKeys(Span, Option<&'src str>), // fn foo(...a)
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Arg<'src> {
    Pos(Span, Expr<'src>),               // foo(1)
    ByName(Span, &'src str, Expr<'src>), // foo(a: 1)
    ExpandPos(Span, Expr<'src>),         // ..[a, b, c]
    ExpandKeys(Span, Expr<'src>),        // ...{a: 1, b: 2}
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Expr<'src> {
    Identifier(Span, &'src str),
    Literal(Span, Literal),
    List(Span, Vec<Expr<'src>>),              // TODO: change to ListItems
    Tuple(Span, Vec<Expr<'src>>),             // tuple literal (1, 2, 3)
    Record(Span, Vec<Field<'src>>),           // record literal { a = 1, b = 2 }
    Prefix(Span, &'src str, Box<Expr<'src>>), // -a
    Infix(Span, Vec<Expr<'src>>, Vec<&'src str>), // 1 - 1
    Call(Span, Box<Expr<'src>>, Vec<Arg<'src>>), // a(b, c)

    Scope(Scope<'src>), // { a }, does not allow public
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
    Module(Module<'src>), // mod {}
    Builtin(Span, &'src str, Vec<Expr<'src>>)
}

type BExpr<'src> = Box<Expr<'src>>;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]

pub struct Scope<'src> {
    pub span: Span,
    pub decl: Vec<Declaration<'src>>,
    pub expr: BExpr<'src>
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]

pub struct Module<'src> {
    pub span: Span,
    pub decl: Vec<Declaration<'src>>
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]

pub struct FnDeclare<'src> {
    pub span: Span,
    pub mods: Vec<DeclareModifier>,
    pub name: &'src str,
    pub params: Vec<Parameter<'src>>,
    pub scope: Expr<'src>
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]

pub struct BlockDeclare<'src> {
    pub span: Span,
    pub mods: Vec<DeclareModifier>,
    pub decls: Vec<Declaration<'src>>
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]

pub struct LetDeclare<'src> {
    pub span: Span,
    pub mods: Vec<DeclareModifier>,
    pub pattern: Pattern<'src>,
    pub binding: Expr<'src>
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]

pub enum DeclareModifier {
    Pub, Rec, Cache
}

// A declaration is a top-level
// type statement/let statement/export statement
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Declaration<'src> {
    Let(LetDeclare<'src>),
    Block(BlockDeclare<'src>),
    Fn(FnDeclare<'src>)
}

impl<'src> Declaration<'src> {
    pub fn add_modifier(&mut self, modifier: DeclareModifier) {
        use Declaration::*;
        match self {
            Let(d) => d.mods.push(modifier),
            Block(d) => d.mods.push(modifier),
            Fn(d) => d.mods.push(modifier)
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ReplInput<'src> {
    Decl(Declaration<'src>),
    Expr(Expr<'src>),
    Command(&'src str)
}

