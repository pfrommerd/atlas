use lang::Primitive;

enum TraceOp {
    Push(Primitive),
    Eq(usize, Primitive),
    Idx(usize, usize),
    Proj(usize, String),
    Call(usize, Vec<usize>)
}

struct Trace {
    func: FuncId,
    nargs: usize,
    ops: Vec<FilterOp>
}

enum Value {
    Primitive(Primitive),
    Partial(FuncId, Vec<Value>),
    Tuple(Vec<Value>),
    Fields(HashMap<String, Value>),
    Unknown
}

struct Result {
    value: Option<Value>,
    satisfies: HashSet<(TableId, TraceId)>
}

struct Table {
    funcs: HashMap<FuncId, TraceTrie>
}