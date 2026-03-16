#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfixOp {
    Add, Sub, Mul,
    Div, Rem, And,
    Or, Xor, Not,
    Shl, Shr, Eq,
    Neq, Lt, Lte,
    Gt, Gte, Cons
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Literal<'src> {
    Integer(u64),
    Char(char),
    String(&'src str),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pattern<'src> {
    Ctr(&'src str),
    Lit(Literal<'src>),
    // []: and <>:
    Nil, Cons,
    // _: wildcard
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Binding<'src> {
    // x or &x for auto-dup
    Var { name: &'src str, dup: bool },
    Dup { label: &'src str, names: Vec<&'src str> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[rustfmt::skip]
pub enum Node<'src> {
    // literal 
    Lit { val: Literal<'src> },
    // List literal [node, node, ...]
    // gets desugared to #Con{node, #Con{node, #Con{node, #Nil}}}
    List { elems: Vec<Node<'src>> },
    /// variable: Foo or Foo#0 or Foo#1 for dup variables
    Var { name: &'src str },
    // reference: @Foo to a name in the book
    Ref { name: &'src str },
    // builtin primitive: %Foo
    Primitive { name: &'src str },
    /// superposition: `"&" Label "{" (Node ",")* Node "}"`
    Sup { label: &'src str, nodes: Vec<Node<'src>> },
    /// duplication term:
    // ! x = y; a + b
    Let { bindings: Vec<(Binding<'src>, Node<'src>)>, body: Box<Node<'src>>, },
    // \ &x -> x + x
    Lambda { binders: Vec<Binding<'src>>, body: Box<Node<'src>>, },
    /// erasure: `"&{}"` or equivalently, `\{}`
    Erase,
    /// constructor: `"#" Name "{" Node,* "}"`
    Construct { name: &'src str, args: Vec<Node<'src>>, },
    /// pattern match: `"?""{" (Pattern "->" Node ";")* Term "}"`
    /// note that ?{Term} or ?{_ -> Term} applies the unboxed value to Term
    /// i.e. ?{\x -> x} #Some{1} ==> 1
    Match { cases: Vec<(Pattern<'src>, Node<'src>)>, default: Option<Box<Node<'src>>> },
    /// f a
    App { func: Box<Node<'src>>, args: Vec<Node<'src>>, },
    /// infix operation: Node Oper Term
    Infix { left: Box<Node<'src>>, op: InfixOp, right: Box<Node<'src>>, },
    /// wildcard: `*`
    Wild,
}