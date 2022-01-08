use super::op::OpAddr;
use super::reg::{RegValue, RegAddr};
use bytes::Bytes;
// use super::op::{CodeReader, RegAddr, OpAddr, Op, OpPrimitive};
use crate::value::{Storage, Pointer};

pub struct Scope {
    // direct values in the registers
    regs : Vec<RegValue>,
    // code: ArenaBox<CodeReader<'a>>,
    code : Pointer,
    cp: OpAddr,
}

impl Scope {
    pub fn empty(code: Pointer) -> Self {
        Scope {
            code,
            regs: Vec::new(),
            // code,
            cp: 0
        }
    }
}

pub struct Machine<'s, S: Storage>  {
    store: &'s mut S,
    stack: Vec<Scope>,
    current: Scope
}

enum OpRes {
    Ok,
    Push(Pointer, OpAddr),
    Jump(Pointer, OpAddr),
    Return(RegAddr)
}

impl<'s, S: Storage> Machine<'s, S> {
    pub fn new(store: &'s mut S, root: Pointer) -> Self {
        Self { store, stack: Vec::new(), 
            current: Scope::empty(root) }
    }

}

// impl<'a> Machine<'a> {
//     pub fn new(arena: &'a Arena,
//                entrypoint: Pointer) -> Self {
//         Machine {
//             arena,
//             stack: Vec::new(),
//             current: Scope::new(entrypoint)
//         }
//     }

//     fn store(scope: &mut Scope, reg: RegAddr, val: OpPrimitive) {

//     }

//     fn run_op(scope: &mut Scope, op: Op) -> OpRes {
//         use Op::*;
//         match op {
//             Store(reg, prim) => panic!("not implemented"),
//             _ => panic!("Unimplemented op type"),
//         }
//     }

//     pub fn run(&mut self) {

//     }
// }