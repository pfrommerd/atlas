use std::collections::VecDeque;

use super::arena::{Arena, Pointer, HeapStorage};
use super::op::{CodeReader, RegAddr, OpAddr, Op, OpPrimitive};

enum Arg {
    Pos(Pointer),
    VarPos(Pointer),
    Named(String, Pointer),
    VarNamed(Pointer)
}

enum RegValue {
    Pointer(Pointer),
    // A unique pointer allows for direct in-place modification
    // rather than allocation of a new object
    Unique(Pointer), 
    Int(i64), // storing integers and floats is optimized
    Float(f64)
}

struct Scope<'a> {
    regs : Vec<Pointer>,
    args: VecDeque<Arg>,

    code: ArenaBox<CodeReader<'a>>,
    cp: OpAddr,
}

impl<'a> Scope<'a> {
    pub fn new(code: Pointer) -> Self {
        Scope {
            regs: Vec::new(),
            args: VecDeque::new(),
            code,
            cp: 0
        }
    }
}

pub struct Machine<'a, H>  where H: HeapStorage {
    arena: &'a Arena<H>,
    stack: Vec<Scope<'a>>,
    current: Scope<'a>
}


enum OpRes {
    Ok,
    Push(Pointer, OpAddr),
    Jump(Pointer, OpAddr),
    Return(RegAddr)
}


impl<'a, H> Machine<'a, H> where H: HeapStorage {
    pub fn new(arena: &'a Arena<H>, entrypoint: Pointer) -> Self {
        Machine {
            arena: arena,
            stack: Vec::new(),
            current: Scope::new(entrypoint)
        }
    }

    fn store(scope: &mut Scope, reg: RegAddr, val: OpPrimitive<'_>) {

    }

    fn run_op(scope: &mut Scope, op: Op<'_>) -> OpRes {
        use Op::*;
        match op {
            Store(reg, prim) => Self::store(scope, reg, prim),
            _ => panic!("Unimplemented op type"),
        }
    }

    pub fn run(&mut self) {

    }
}