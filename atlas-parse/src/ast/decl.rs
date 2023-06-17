use super::types::{Type, Pattern};
use super::expr::{Expr, ExprBlock};

#[derive(Debug, Clone)]
pub enum Modifier {
    Pub
}

#[derive(Debug, Clone)]
pub struct LetDecl<'src> {
    pub modifier: Option<Modifier>,
    pub pattern: Pattern<'src>,
    pub value: Expr<'src>
}

#[derive(Debug, Clone)]
pub struct FnDecl<'src> {
    pub modifier: Option<Modifier>,
    pub name: &'src str,
    pub args: Vec<Pattern<'src>>,
    pub body: ExprBlock<'src>
}

#[derive(Debug, Clone)]
pub enum EnumVariant<'src> {
    Tuple(&'src str, Vec<Type<'src>>),
    Struct(&'src str, Vec<(&'src str, Type<'src>)>),
    Empty(&'src str)
}

#[derive(Debug, Clone)]
pub struct EnumDecl<'src> {
    pub modifier: Option<Modifier>,
    pub name: &'src str,
    pub variants: Vec<EnumVariant<'src>>
}

#[derive(Debug, Clone)]
pub struct StructDecl<'src> {
    pub modifier: Option<Modifier>,
    pub name: &'src str,
    pub entries: Vec<(&'src str, Type<'src>)>
}

#[derive(Debug, Clone)]
pub struct TraitDecl<'src> {
    pub modifier: Option<Modifier>,
    pub name: &'src str,
}

#[derive(Debug, Clone)]
pub struct ImplDecl<'src> {
    pub modifier: Option<Modifier>,
    pub name: &'src str,
}

#[derive(Debug, Clone)]
pub struct AliasDecl<'src> {
    pub modifier: Option<Modifier>,
    pub lhs: Type<'src>,
    pub rhs: Type<'src>
}

#[derive(Debug, Clone)]
pub struct ModDecl<'src> {
    pub modifier: Option<Modifier>,
    pub name: &'src str,
    pub value: Module<'src>
}

#[derive(Debug, Clone)]
pub enum Declaration<'src> {
    Mod(ModDecl<'src>),
    Let(LetDecl<'src>),
    Fn(FnDecl<'src>),
    // Types
    Alias(AliasDecl<'src>),
    Enum(EnumDecl<'src>),
    Struct(StructDecl<'src>),
    Trait(TraitDecl<'src>),
    Impl(ImplDecl<'src>)
}

#[derive(Debug, Clone)]
pub struct Module<'src> {
    pub decls: Vec<Declaration<'src>>
}

