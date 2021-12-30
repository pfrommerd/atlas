use super::op::{Code, Op, OpPtr, PrimitiveOp, RegAddr};
use super::value::{Heap, Register, Scope, Value, ValuePtr};
use crate::core::lang::Primitive;
use std::rc::Rc;

pub struct Machine<'h> {
    pub heap: &'h mut Heap,
    stack: Vec<(Dest, Rc<Code>, OpPtr, Scope)>,
}

#[derive(Clone, Copy)]
pub enum Dest {
    // where the destination of a force should be written into
    Reg(RegAddr),
    Ptr(ValuePtr),
}

pub enum StackOp {
    Force(Dest, Rc<Code>, OpPtr, Scope), // dest, code, offset, execution scope
    Ret(Register),
    Done,
}

impl<'h> Machine<'h> {
    pub fn new(heap: &'h mut Heap) -> Machine<'h> {
        Machine {
            heap,
            stack: Vec::new(),
        }
    }

    pub fn push(&mut self, thunk: ValuePtr) {
        let (code, op, scope) = match self.heap.at(thunk) {
            Value::Thunk(c, o, s) => (c, o, s.clone()),
            _ => panic!("Unexpected non-thunk being pushed"),
        };
        self.stack
            .push((Dest::Ptr(thunk), Rc::clone(code), *op, scope))
    }

    pub fn run(&mut self) {
        while !self.stack.is_empty() {
            let (d, code_rc, op, scope) = self.stack.last_mut().unwrap();
            let dest = *d;

            // dereference the current code block we are in
            // so that we don't do an rc inc/dec for every op
            match Self::exec_scope(&mut self.heap, &code_rc, op, scope) {
                StackOp::Force(dest, c, o, s) => {
                    self.stack.push((dest, c, o, s));
                }
                StackOp::Ret(reg) => {
                    self.stack.pop();
                    match dest {
                        Dest::Reg(r) => {
                            // copy the returned value directly into the top
                            // scope's register
                            let s = &mut self.stack.last_mut().unwrap().3;
                            s.set(r, reg);
                        }
                        Dest::Ptr(p) => {
                            self.heap.set_or_copy(p, reg);
                        }
                    }
                }
                StackOp::Done => {
                    self.stack.pop();
                }
            }
        }
    }

    // will continue executing instructions
    // until we get to a "force" or a "ret"
    // will not return "ptr"
    pub fn exec_scope(
        heap: &mut Heap,
        code: &Rc<Code>,
        ptr: &mut OpPtr,
        scope: &mut Scope,
    ) -> StackOp {
        loop {
            if *ptr >= code.len() {
                return StackOp::Done;
            };
            let (op, res) = Self::exec_op(heap, code, *ptr, scope);
            *ptr = op;
            match res {
                Some(s) => return s,
                None => (),
            }
        }
    }

    fn exec_op(
        heap: &mut Heap,
        code: &Rc<Code>,
        ptr: OpPtr,
        scope: &mut Scope,
    ) -> (OpPtr, Option<StackOp>) {
        let op = code.at(ptr);
        use Op::*;
        match op {
            Force(reg) => {
                let r = scope.take_reg(*reg);
                match r {
                    Register::Ptr(dest) => match heap.at(dest) {
                        Value::Thunk(code, off, scope) => {
                            return (
                                ptr + 1,
                                Some(StackOp::Force(
                                    Dest::Ptr(dest),
                                    Rc::clone(code),
                                    *off,
                                    scope.clone(),
                                )),
                            )
                        }
                        _ => return (ptr + 1, None),
                    },
                    Register::Value(v) => match v {
                        Value::Thunk(code, off, scope) => {
                            return (
                                ptr + 1,
                                Some(StackOp::Force(Dest::Reg(*reg), code, off, scope)),
                            )
                        }
                        _ => return (ptr + 1, None),
                    },
                    Register::Empty => panic!("Cannot force an empty value"),
                }
            }
            Ret(reg) => {
                let r = scope.take_reg(*reg);
                return (ptr, Some(StackOp::Ret(r)));
            }
            Done => return (ptr, Some(StackOp::Done)),
            // Primitive ops
            Prim(reg, prim) => {
                // TODO: Literals should be compiled into some kind of "literalid"
                // where the code contains a lookup table to pointers
                scope.set(*reg, Register::Value(Value::Primitive(prim.clone())));
            }
            PrimitiveOp(op) => {
                Self::exec_prim_op(heap, op, scope);
            }
            // Structure-related ops

            // Argument packing ops
            // Argument Unpacking ops

            // Entrypoint ops
            Entrypoint(reg, ptr) => scope.set(
                *reg,
                Register::Value(Value::Entrypoint(Rc::clone(code), *ptr, Scope::new())),
            ),
            // Handle
            _ => panic!("Op type not implemented: {:?}", op),
        }
        (ptr + 1, None)
    }

    fn exec_prim_op(heap: &mut Heap, op: &PrimitiveOp, scope: &mut Scope) {
        use Primitive::*;
        use PrimitiveOp::*;
        match op {
            Add(dest, a, b) => {
                let a_val = scope.get_heaped(heap, *a);
                let b_val = scope.get_heaped(heap, *b);
                let (a_prim, b_prim) = match (a_val, b_val) {
                    (Value::Primitive(a_prim), Value::Primitive(b_prim)) => (a_prim, b_prim),
                    _ => panic!("Expected primitive arguments"),
                };
                let res = match (a_prim, b_prim) {
                    (Int(l), Int(r)) => Int(l + r),
                    _ => panic!("Bad primitive types"),
                };
                scope.set_value(*dest, Value::Primitive(res))
            }
            _ => panic!("Unimplemented primitive op"),
        }
    }
}
