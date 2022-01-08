use std::collections::VecDeque;

// use crate::value::{
//     Pointer,
// };
// use super::op::{CodeReader, RegAddr, OpAddr, Op, OpPrimitive};

// enum RegValue {
//     Unit,
//     Float(f64),
//     Int(i64),
//     Bool(bool),
//     Char(char),
//     String(String),
//     Buffer(Bytes),

//     Indirect(Pointer),

//     // an external type, annotated with
//     // a identifier string and a blob
//     External(&'static str, Bytes),

//     Partial(Partial),
//     Thunk(Partial),

//     // head, tail
//     Nil,
//     Cons(Pointer, Pointer),
//     Record(HashMap<String, Pointer>),
//     Tuple(Vec<Pointer>),
//     Variant(Pointer, Pointer)
// }

// struct Scope {
//     // direct values in the registers
//     regs : Vec<RegValue>,
//     args: VecDeque<Arg>,

//     // code: ArenaBox<CodeReader<'a>>,
//     cp: OpAddr,
// }

// impl Scope {
//     pub fn new(code: Pointer) -> Self {
//         Scope {
//             regs: Vec::new(),
//             args: VecDeque::new(),
//             // code,
//             cp: 0
//         }
//     }
// }

// pub struct Machine<'a>  {
//     arena: &'a Arena,
//     stack: Vec<Scope>,
//     current: Scope
// }

// enum OpRes {
//     Ok,
//     Push(Pointer, OpAddr),
//     Jump(Pointer, OpAddr),
//     Return(RegAddr)
// }

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