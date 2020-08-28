use super::node::{Node, NodePtr, Heap, PrimitiveOp, Primitive};
use std::fmt;

// graph reduction template-instatiation machine
pub struct TiMachine<'mach, 'heap> {
    heap: &'mach mut Heap<'heap>,
    stack: Vec<NodePtr<'heap>>,
    dump: Vec<Vec<NodePtr<'heap>>>
}

impl<'mach, 'heap> TiMachine<'mach, 'heap> {
    pub fn new(heap: &'mach mut Heap<'heap>, root: NodePtr<'heap>) -> Self {
        let mut stack = Vec::new();
        stack.push(root);
        return TiMachine { heap, stack, dump: Vec::new() }
    }

    fn dump_for(&mut self, n: NodePtr<'heap>) {
        let mut stack = Vec::new();
        std::mem::swap(&mut stack, &mut self.stack);
        self.dump.push(stack);
        self.stack.push(n);
    }
    // Do a step on the current stack
    fn stack_step(&mut self) -> bool {
        use Node::*;
        if let Some(&node_ptr) = self.stack.last() {
            // rewrite any indirections
            while let &Indirection(ptr) = self.heap.at(node_ptr) {
                let rewrite = self.heap.at(ptr).clone();
                self.heap.set(node_ptr, rewrite);
            }
            let node = self.heap.at(node_ptr);
            if let &App(left, _) = node {
                self.stack.push(left);
                return true;
            }
            if let Some(arity) = node.direct_arity() {
                if (1 + arity as usize) < self.stack.len() {
                    return false;
                }
                // grab a copy of the root pointer
                // before we split off
                let root_ptr = *self.stack.first().unwrap();
                let lam_ptr = self.stack.pop().unwrap();

                // extract the args from the stack
                let stack_args = self.stack.split_off(self.stack.len() - arity as usize);
                let iter = stack_args.iter().rev();

                let args : Vec<NodePtr<'heap>> = iter.map(|&x| 
                    if let &App(_, y) = self.heap.at(x) { 
                        y 
                    } else { 
                        panic!("Stack must have Apps")
                }).collect();

                // push the root back onto the stack
                self.stack.push(root_ptr);

                // resolve any argument indirections
                for &arg_ptr in args.iter() {
                    while let &Indirection(ptr) = self.heap.at(arg_ptr) {
                        let rewrite = self.heap.at(ptr).clone();
                        self.heap.set(arg_ptr, rewrite);
                    }
                }

                use PrimitiveOp::*;
                use Primitive::*;

                let lam = self.heap.at(lam_ptr).clone();
                let result = match lam {
                    Combinator(_, body_ptr) => {
                        let body_copy_ptr : NodePtr<'heap> = self.heap.copy(body_ptr);
                        self.heap.replace_args(body_copy_ptr, lam_ptr, &args);
                        self.heap.at(body_copy_ptr).clone()
                    },
                    PrimOp(op) => {
                        let mut binary_op = |f : fn(Primitive, Primitive) -> Option<Primitive>| {
                            let left = self.heap.at(args[0]);
                            let right = self.heap.at(args[1]);
                            if let Prim(la) = left {
                                if let Prim(ra) = right {
                                    Some(Prim(f(*la, *ra)?))
                                } else {
                                    self.dump_for(args[1]);
                                    None
                                }
                            } else {
                                self.dump_for(args[0]);
                                None
                            }
                        };
                        match op {
                            IAdd => binary_op(|l : Primitive, r : Primitive| { 
                                let a = l.as_int()?; 
                                let b = r.as_int()?; 
                                Some(Int(a + b))
                            }).unwrap_or(Bad),
                            ISub => binary_op(|l : Primitive, r : Primitive| { 
                                let a = l.as_int()?; 
                                let b = r.as_int()?; 
                                Some(Int(a - b))
                            }).unwrap_or(Bad),
                            IMul => binary_op(|l : Primitive, r : Primitive| { 
                                let a = l.as_int()?; 
                                let b = r.as_int()?; 
                                Some(Int(a * b))
                            }).unwrap_or(Bad),
                            IDiv => binary_op(|l : Primitive, r : Primitive| { 
                                let a = l.as_int()?; 
                                let b = r.as_int()?; 
                                Some(Int(a / b))
                            }).unwrap_or(Bad),
                            _ => panic!("Unhandled primtiive op")
                        }
                    },
                    Pack(tag, _, dtype) => Data(tag, args, dtype),
                    _ => panic!("Unhandled combinator type")
                };
                if let Bad = result {
                    return true
                }
                // rewrite the root node to be the result
                self.heap.set(root_ptr, result);
            }
        }
        false
    }

    pub fn step(&mut self) -> bool {
        // Try doing a step from  the stack, pop a stack from the dump
        // and continue there if we can't do more on the current stack
        if !self.stack_step() {
            if let Some(stack) = self.dump.pop() {
                self.stack = stack;
                return true
            } else {
                return false
            }
        }
        true
    }

    pub fn result(&self) -> NodePtr<'heap> {
        *self.stack.iter().nth(0).unwrap()
    }

    pub fn run(&mut self) -> NodePtr<'heap> {
        while self.step() {}
        // get the top of the stack
        *self.stack.iter().nth(0).unwrap()
    }
}

impl fmt::Display for TiMachine<'_, '_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        writeln!(f, "Heap:")?;
        writeln!(f, "{}", self.heap)?;
        writeln!(f, "Dump:")?;
        for (i, frame) in self.dump.iter().enumerate() {
            writeln!(f, "Dump {}", i)?;
            for (j, elem) in frame.iter().enumerate() {
                writeln!(f, "{}: {}", self.heap.ptr(j), self.heap.at(*elem))?;
            }
            writeln!(f, "")?;
        }
        writeln!(f, "")?;
        writeln!(f, "Stack:")?;
        for (i, elem) in self.stack.iter().enumerate() {
            writeln!(f, "{}: {}", self.heap.ptr(i), self.heap.at(*elem))?;
        }
        write!(f, "")?;
        Ok(())
    }

}

#[cfg(test)]
mod test {
    use super::super::node::{
        Node, Heap, PrimitiveOp, Primitive
    };
    use super::TiMachine;

    #[test]
    fn simple_addition() {
        // make the expr (+ 1) 2
        let mut heap = Heap::new();

        let left = heap.add(Node::Prim(Primitive::Int(1)));
        let right = heap.add(Node::Prim(Primitive::Int(2)));
        let add = heap.add(Node::PrimOp(PrimitiveOp::IAdd));

        let app = heap.add(Node::App(add, left));
        let root = heap.add(Node::App(app, right));

        let mut machine = TiMachine::new(heap, root);
        let result_ptr = machine.run();
        let result = machine.heap.at(result_ptr).clone();
        if let Node::Prim(Primitive::Int(val)) = result {
            assert_eq!(3, val);
        } else {
            panic!("Expected the result to be 3!");
        }
    }

    #[test]
    fn addition_end_to_end() {

    }
}