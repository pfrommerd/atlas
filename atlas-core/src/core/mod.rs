type Symbol = String;

#[derive(Debug,Clone)]
enum Literal {
    Integer(i64),
    Float(f64),
    Bool(bool),
    String(String),
    Unit
}

#[derive(Debug,Clone)]
struct Let {
    sym: Symbol,
    val: Expr,
    body: Expr
}

#[derive(Debug,Clone)]
struct Bind {
    lam: Expr,
    args: Vec<Expr>
}

#[derive(Debug,Clone)]
struct Invoke {
    lam: Expr
}

#[derive(Debug,Clone)]
enum Expr {
    Literal(Literal),
    Var(Symbol),
    Let(Box<Let>),
    Bind(Box<Bind>),
    Invoke(Box<Invoke>)
}