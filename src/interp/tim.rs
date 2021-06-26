use super::node::{NodePtr, Heap};

// graph reduction template-instatiation machine
pub struct TiMachine<'mach, 'heap> {
    pub heap: &'mach mut Heap<'heap>,
    pub stack: Vec<NodePtr<'heap>>,
    pub dump: Vec<Vec<NodePtr<'heap>>>
}

/*

pub struct Frame<'heap> {
    call_node: NodePtr<'heap>,
    lam_node: NodePtr<'heap>,
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

    pub fn dump_for(&mut self, n: NodePtr<'heap>) -> bool {
        if self.heap.at(n).is_whnf(self.heap) {
            return false;
        }
        let mut stack = Vec::new();
        std::mem::swap(&mut stack, &mut self.stack);
        self.dump.push(stack);
        self.stack.push(n);
        return true;
    }

    // Evaluate to whnf, removing any indirections
    fn eval(&mut self, n: NodePtr<'heap>) -> NodePtr<'heap> {
        if self.heap.at(n).is_whnf(self.heap) {
            let mut x = n;
            while let Node::Ind(p) = self.heap.at(x) { x = *p; }
            return x;
        }
        let mut stack = Vec::new();
        std::mem::swap(&mut stack, &mut self.stack);
        self.stack.push(n);
        while self.stack_step() {}
        std::mem::swap(&mut stack, &mut self.stack);
        let mut x = n;
        while let Node::Ind(p) = self.heap.at(x) { x = *p; }
        x
    }

    fn exec(&mut self, lam: Node<'heap>, root: NodePtr<'heap>, args: Vec<NodePtr<'heap>>) {
        use Node::*;
        match lam {
            Combinator(body_ptr) => {
                self.heap.instantiate_at(body_ptr, root, &args);
            },
            Foreign(f) => {
                let res = f.2(self, args);
                if let Some(n) = res {
                    self.heap.set(root, n);
                }
            },
            PrimOp(p) => {
                match p.arity() {
                    1 => {
                        if self.dump_for(args[0]) { return; }
                        let ptr= args[0];
                        let arg= match self.heap.at(ptr) {
                            Prim(p) => p,
                            _ => panic!("left value must be a primitive")
                        };
                        let res = p.eval_unary(arg);
                        match res {
                            Some(r) => self.heap.set(root, Node::Prim(r)),
                            None => panic!("bad arguments")
                        }
                    },
                    2 => {
                        if self.dump_for(args[0]) { return; }
                        if self.dump_for(args[1]) { return; }
                        let left_ptr = args[0];
                        let right_ptr  = args[1];
                        let left = match self.heap.at(left_ptr) {
                            Prim(p) => p,
                            _ => panic!("left value must be a primitive")
                        };
                        let right = match self.heap.at(right_ptr) {
                            Prim(p) => p,
                            _ => panic!("left value must be a primitive")
                        };
                        let res = p.eval_binary(left, right);
                        match res {
                            Some(r) => self.heap.set(root, Node::Prim(r)),
                            None => panic!("bad arguments")
                        }
                    },
                    _ => panic!("Unhandled number of PrimOp args")
                }
            },
            Pack(tag, _, fmt) => {
                self.heap.add(Node::Data(tag, args, fmt));
            },
            Case(conds) => {
                if self.dump_for(args[0]) { return; }
                let n = self.heap.at(args[0]);
                match n {
                Prim(p) => {
                    for (c, n) in conds {
                        match c {
                        Cond::Eq(v) => if v == p { self.heap.set(root, Node::Ind(n)); return; },
                        Cond::Tag(_) => panic!("Cannot scrutinize primitive by tag"),
                        Cond::Default => { self.heap.set(root, Node::Ind(n)); return; }
                        }
                    }
                },
                Data(a, _) => {
                    for (c, n) in conds {
                        match c {
                        Cond::Eq(v) => panic!("Cannot scrutinize data by eq"),
                        Cond::Tag(t) => if a == t { self.heap.set(root, n); return; },
                        Cond::Default => { self.heap.set(root, Node::Ind(n)); return; }
                        }
                    }
                },
                _ => panic!("Cannot scrutinize argument by case")
                }
            },
            _ => panic!("Unable to execute non-lambda")
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
        let arity = match node.arity() {
            None => return false,
            Some(a) => a
        };
        let args = std::cmp::max(arity, self.stack.len() - 1);
        // grab a copy of the root pointer
        // before we split off
        let root = self.stack[self.stack.len() - 1 - args];
        self.stack.pop(); // pop the lambda pointer off the stack
        let mut stack_args : Vec<NodePtr<'heap>> = self.stack.split_off(self.stack.len() - args)
            .iter().map(|&x|
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
        self.stack.push(root);
        let mut lam_ptr = node_ptr;
        match self.heap.at(node_ptr) {
            Partial(_, f, args) => {
                lam_ptr = *f;
                while let Ind(p) = self.heap.at(lam_ptr) {
                    lam_ptr = *p;
                }
                stack_args.extend(args.iter().rev());
            },
            _ => ()
        }
        stack_args.reverse();
        let lam = self.heap.at(lam_ptr).clone();
        // get the arity of the underlying lambda
        let arity = match lam.arity() {
            None => panic!("Lambda must have an arity"),
            Some(a) => a
        };
        if arity > stack_args.len() {
            self.heap.set(root, Node::Partial(arity, lam_ptr, stack_args))
        } else {
            debug_assert_eq!(arity, stack_args.len());
            self.exec(lam, root, stack_args);
        }
        return true
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
        Node, Heap, PrimitiveOp
    };
    use crate::core::lang::Primitive;
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
    */