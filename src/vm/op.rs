use crate::core::lang::Primitive;
pub type RegAddr = usize;

#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub enum PrimitiveOp {
    Negate(RegAddr, RegAddr),
    Add(RegAddr, RegAddr, RegAddr),
    Sub(RegAddr, RegAddr, RegAddr),
    Mul(RegAddr, RegAddr, RegAddr),
    Div(RegAddr, RegAddr, RegAddr),
    Mod(RegAddr, RegAddr, RegAddr),
    Or(RegAddr, RegAddr, RegAddr),
    And(RegAddr, RegAddr, RegAddr),
    Xor(RegAddr, RegAddr, RegAddr),
}

#[derive(Debug)]
pub enum Op {
    Force(RegAddr), // register to eval
    Ret(RegAddr),   // register to return
    Done,           // stop executing the current stack
    // should be used only for evaluating SymbolEnv's
    // in repl mode

    // will copy the register itself
    Cp(RegAddr, RegAddr), // dest, src

    Reserve(RegAddr), // reserve an address on the heap
    // use a reservation. RHS must be local, both RHS, LHS will be modified
    UseReserve(RegAddr, RegAddr),

    // value constructors
    Prim(RegAddr, Primitive), // store primtiive into register
    PrimitiveOp(PrimitiveOp), // executes a primitive op

    // for entrypoints only!
    // entrypoint (direct),
    PosArg(RegAddr, RegAddr),
    PosVarArg(RegAddr, RegAddr),
    KeyArg(RegAddr, String, RegAddr),
    KeyVarArg(RegAddr, RegAddr),

    // takes a position argument, looks at the key args
    // if no position args are left
    ExPosArg(RegAddr),
    ExNamedArg(RegAddr, String),
    ExOptNamedArg(RegAddr, String),
    // extracts the remaining position args to a list
    ExPosVarArg(RegAddr),
    // extracts the remaining key args
    // there cannot be any positional args remaining
    ExKeyVarArg(RegAddr),

    // will be compiled into a regular entrypoint
    // used during compilation because we don't know
    // how big everything will be
    EntrypointSeg(RegAddr, SegmentId), // dest, segment id
    Entrypoint(RegAddr, OpPtr),        // dest, address
    PushReg(RegAddr, RegAddr),         // push reg onto entrypoint

    JmpSegIf(RegAddr, SegmentId), // register, target segment id
    JmpIf(RegAddr, OpPtr),        // register, target address

    Thunk(RegAddr, RegAddr), // dest, entrypoint (must be direct)
}

pub type OpPtr = usize;

#[derive(Debug)]
pub struct Code {
    ops: Vec<Op>,
}

impl Code {
    pub fn new(c: Vec<Op>) -> Code {
        Code { ops: c }
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    pub fn at(&self, p: OpPtr) -> &Op {
        &self.ops[p]
    }
}

type SegmentId = usize;

pub struct SegmentBuilder {
    pub id: SegmentId,
    code: Code,
    next_reg: RegAddr, // next free register for the scope in this segment
}

impl SegmentBuilder {
    pub fn append(&mut self, op: Op) {
        self.code.ops.push(op)
    }
    pub fn extend(&mut self, ops: Vec<Op>) {
        self.code.ops.extend(ops)
    }
    pub fn next_reg(&mut self) -> RegAddr {
        let r = self.next_reg;
        self.next_reg = self.next_reg + 1;
        r
    }
}

pub struct CodeBuilder {
    segs: Vec<Option<SegmentBuilder>>, // the segments
}

impl CodeBuilder {
    pub fn new() -> CodeBuilder {
        CodeBuilder { segs: Vec::new() }
    }

    pub fn next<'a>(&mut self) -> SegmentBuilder {
        let id = self.segs.len();
        self.segs.push(Option::None);
        SegmentBuilder {
            id,
            code: Code { ops: Vec::new() },
            next_reg: 0,
        }
    }
    pub fn register(&mut self, seg: SegmentBuilder) {
        let id = seg.id;
        self.segs[id] = Some(seg)
    }

    pub fn build(self) -> (Code, Vec<usize>) {
        let mut ops = Vec::new();
        let mut segment_locs = Vec::new();
        for sb in self.segs {
            segment_locs.push(ops.len());
            match sb {
                None => panic!("Unregistered segment referenced!"),
                Some(s) => ops.extend(s.code.ops),
            }
        }
        // now we replace all of the segment jmp/entrypointseg
        for item in ops.iter_mut() {
            match item {
                &mut Op::EntrypointSeg(reg, seg) => *item = Op::Entrypoint(reg, segment_locs[seg]),
                &mut Op::JmpSegIf(cond, seg) => *item = Op::JmpIf(cond, segment_locs[seg]),
                _ => (),
            }
        }
        (Code { ops }, segment_locs)
    }
}
