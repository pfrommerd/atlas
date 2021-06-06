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
        return TiMachine { 
            heap, stack, 
            dump: Vec::new()
        }
    }

    fn dump_for(&mut self, n: NodePtr<'heap>) {
        let mut stack = Vec::new();
        std::mem::swap(&mut stack, &mut self.stack);
        self.dump.push(stack);
        self.stack.push(n);
    }

    // Will determine if a given ptr is in weak-head normal form
    fn is_whnf(&self, n: NodePtr<'heap>) -> bool {
        use Node::*;
        match self.heap.at(n) {
            App(_, _) => false, // if there is an apply either we evaluate 
                                // or turn into an unsaturated (which is whnf)
            Ind(p) => self.is_whnf(*p),
            _ => true
        }
    }

    // Do a step on the current stack
    fn stack_step(&mut self) -> bool {
        use Node::*;
        let mut node_ptr = match self.stack.last() {
            Some(&n) => n,
            None => return false
        };
        while let &Ind(ptr) = self.heap.at(node_ptr) {
            node_ptr = ptr;
        }

        let node = self.heap.at(node_ptr);
        if let App(left, _) = node {
            self.stack.push(*left);
            return true;
        }
        if let Some(arity) = node.arity() {
            if (1 + arity as usize) > self.stack.len() {
                return false;
            }
            // grab a copy of the root pointer
            // before we split off
            let root_ptr = self.stack[self.stack.len() - 1 - arity as usize];
            let lam_ptr = node_ptr;
            self.stack.pop(); // pop the lambda pointer off the stack

            // extract the args from the stack
            let stack_args = self.stack.split_off(self.stack.len() - arity as usize);
            let iter = stack_args.iter().rev();

            let args : Vec<NodePtr<'heap>> = iter.map(|&x| 
                if let &App(_, y) = self.heap.at(x) { 
                    let mut arg = y;
                    // resolve any argument indirections
                    while let &Ind(ptr) = self.heap.at(arg) {
                        arg = ptr;
                    }
                    arg
                } else { 
                    panic!("Stack must have Apps")
            }).collect();

            // push the root back onto the stack
            self.stack.push(root_ptr);

            use PrimitiveOp::*;
            use Primitive::*;

            let lam = self.heap.at(lam_ptr).clone();
            let result = match lam {
                Combinator(_, body_ptr) => {
                    self.heap.instantiate_at(body_ptr, root_ptr, &args);
                    return true;
                },
                PrimOp(op) => {
                    let mut binary_op = |f : fn(Primitive, Primitive) -> Option<Primitive>| {
                        let left = self.heap.at(args[0]);
                        let right = self.heap.at(args[1]);
                        if let Prim(la) = left {
                            if let Prim(ra) = right {
                                Some(Prim(f(*la, *ra)?))
                            } else if !self.is_whnf(args[1]) {
                                self.dump_for(args[1]);
                                None
                            } else {
                                None
                            }
                        } else if !self.is_whnf(args[0]) {
                            self.dump_for(args[0]);
                            None
                        } else {
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
                Pack(tag, _, f) => Data(tag, args, f),
                _ => {
                    print!("Lambda {:?}", lam);
                    panic!("Unhandled combinator type")
                }
            };
            if let Bad = result {
                return true
            }
            // rewrite the root node to be the result
            self.heap.set(root_ptr, result);
            return true
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
        let mut res = *self.stack.iter().nth(0).unwrap();
        while let Node::Ind(p) = self.heap.at(res) {
            res = *p;
        }
        res
    }

    pub fn run(&mut self) -> NodePtr<'heap> {
        while self.step() {}
        // get the top of the stack
        self.result()
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
        for elem in self.stack.iter() {
            writeln!(f, "{}: {}", elem, self.heap.at(*elem))?;
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

        let mut machine = TiMachine::new(&mut heap, root);
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