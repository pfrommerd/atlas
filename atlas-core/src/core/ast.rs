
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfixOp {
    Add, Sub, Mul,
    Div, Rem, And,
    Or, Xor, Not,
    Shl, Shr, Eq,
    Neq, Lt, Lte,
    Gt, Gte,
    Cons
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
#[rustfmt::skip]
pub enum Node<'src> {
    // literal 
    Lit { val: Literal<'src> },
    // List builtin [node, node, ...]
    // gets desugared to #Con{node, #Con{node, #Con{node, #Nil}}}
    List { elems: Vec<Node<'src>> },
    /// variable: `Name`
    Var { name: &'src str },
    /// first dup variable: `Name "₀"`
    Dp0 { name: &'src str },
    /// second dup variable: `Name "₁"`
    Dp1 { name: &'src str },
    /// reference: `"@" Name`
    Ref { name: &'src str },
    /// primitive (native) function: `"%Name`
    Pri { name: &'src str },
    /// name (stuck head): `"^" Name`
    Nam { name: &'src str },
    /// dry (stuck application): `"^" "(" Node " " Term ")"`
    Dry { func: Box<Node<'src>>, arg: Box<Node<'src>>, },
    /// superposition: `"&" Label "{" Node "," Term "}"`
    Sup { label: &'src str, left: Box<Node<'src>>, right: Box<Node<'src>>, },
    /// duplication term: `"!" Name "&" Label "=" Node ";" Term`
    Dup { name: &'src str, label: &'src str, val: Box<Node<'src>>, nxt: Box<Node<'src>>, },
    /// constructor: `"#" Name "{" Node,* "}"`
    Ctr { name: &'src str, args: Vec<Node<'src>>, },
    /// pattern match: `"λ" "{" (Pattern ":" Node ";")* Term "}"`
    Mat { cases: Vec<(Pattern<'src>, Node<'src>)>, default: Option<Box<Node<'src>>> },
    /// use (unbox): `λ{ Node }` or `λ {_: Term }`
    Use { term: Box<Node<'src>> },
    /// erasure: `"&{}"` or equivalently, `λ{}`
    Era,
    /// lambda: `"λ" Name* "." Node`
    Lam { names: Vec<&'src str>, body: Box<Node<'src>>, },
    /// application: `"(" Node " " Term ")"`
    App { func: Box<Node<'src>>, args: Vec<Node<'src>>, },
    /// infix operation: `"(" Node Oper Term ")"`
    Infix { left: Box<Node<'src>>, op: InfixOp, right: Box<Node<'src>>, },
    /// dynamic superposition: `"&" "(" Node ")" "{" Term "," Term "}"`
    DSup { label: Box<Node<'src>>, left: Box<Node<'src>>, right: Box<Node<'src>>, },
    /// dynamic duplication term: `"!" Name "&" "(" Node ")" "=" Term ";" Term`
    DDup { name: &'src str, label: Box<Node<'src>>, val: Box<Node<'src>>, nxt: Box<Node<'src>>, },
    /// priority wrapper: `"↑" Node`
    Inc { term: Box<Node<'src>> },
    /// allocation: `"@" "{" Name,* "}" Node`
    Alo { names: Vec<&'src str>, nxt: Box<Node<'src>>, },
    /// unscoped binding: `"!" "$" "{" Name "," Name "}" ";" Node`
    Uns { name1: &'src str, name2: &'src str, nxt: Box<Node<'src>>, },
    /// wildcard / any: `"*"`
    Any,
}