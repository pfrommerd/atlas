
#[derive(Debug, Clone)]
pub enum Type<'src> {
    Identifier(&'src str)
}

#[derive(Debug, Clone)]
pub enum Pattern<'src> {
    Identifier(&'src str),
    Typed(Box<Pattern<'src>>, Type<'src>),
}
