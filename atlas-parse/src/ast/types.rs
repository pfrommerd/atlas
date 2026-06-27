use super::expr::Literal;

#[derive(Debug, Clone)]
pub enum Type<'src> {
    Identifier(&'src str)
}

#[derive(Debug, Clone)]
pub enum Pattern<'src> {
    Identifier(&'src str),
    Wildcard,
    Literal(Literal<'src>),
    Constructor(&'src str, Vec<Pattern<'src>>),
    Tuple(Vec<Pattern<'src>>),
    Typed(Box<Pattern<'src>>, Type<'src>),
}
