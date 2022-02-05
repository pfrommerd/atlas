use super::ast::Literal;

use pretty::{DocBuilder, DocAllocator, Doc, Pretty};


impl Pretty<D, A> for &'a Literal
    where
        D: DocAllocator<'b, A>,
        D::Doc: Clone,
        A: 'a {
    fn pretty(self, allocator: &'b D) -> DocBuilder<'b, D, A> 
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
