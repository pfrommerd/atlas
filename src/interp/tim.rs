use super::node::{Node, NodePtr, Heap, PrimitiveOp, Primitive};

// graph reduction template-instatiation machine
pub struct TiMachine<'heap> {
    heap: Heap<'heap>,
    stack: Vec<NodePtr<'heap>>
}

impl<'heap> TiMachine<'heap> {
    pub fn new(heap: Heap<'heap>, root: NodePtr<'heap>) -> Self {
        let mut stack = Vec::new();
        stack.push(root);
        return TiMachine { heap, stack }
    }
    pub fn step(&mut self) -> bool {
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
                        match op {
                            IAdd => if let (&Prim(Int(left)), &Prim(Int(right))) = 
                                           (self.heap.at(args[0]), self.heap.at(args[1])) {
                                Prim(Int(left + right))
                            } else { panic!("Bad arguments to IAdd") },
                            _ => panic!("Unhandled primtiive op")
                        }
                    },
                    Pack(tag, arity, dtype) => panic!("Unhandled pack"),
                    _ => panic!("Unhandled combinator type")
                };
                // rewrite the root node to be the result
                self.heap.set(root_ptr, result);
            }
        }
        false
    }

    pub fn run(&mut self) -> NodePtr<'heap> {
        while self.step() {}
        // get the top of the stack
        *self.stack.iter().nth(0).unwrap()
    }
}

#[cfg(test)]
mod test {
    use super::super::node::{
        Node, NodePtr, Heap, PrimitiveOp, Primitive
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

}