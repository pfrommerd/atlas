use super::graph::{LamGraph, OpNode, CompRef};
use crate::value::{Storage, StorageError, ObjectRef};
use crate::vm::op::{ObjectID, DestBuilder, OpBuilder};
use std::collections::{VecDeque, HashMap, HashSet};
use std::ops::Deref;

pub struct CompileError {

}

impl From<StorageError> for CompileError {
    fn from(_: StorageError) -> Self {
        Self {}
    }
}

struct IDMapping {
    in_edges: HashMap<CompRef, Vec<CompRef>>,
    map: HashMap<CompRef, ObjectID>,
    last: ObjectID
}

impl IDMapping {
    fn new() -> Self {
        Self {
            in_edges: HashMap::new(),
            map: HashMap::new(),
            last: 0
        }
    }

    fn add_dep(&mut self, parent: CompRef, child: CompRef) {
        self.in_edges.entry(child).or_insert(Vec::new()).push(parent);
    }

    fn get(&self, c: CompRef) -> Result<ObjectID, CompileError> {
        self.map.get(&c).cloned().ok_or(CompileError {})
    }

    fn build_dest(&self, dest: CompRef, mut builder: DestBuilder) -> Result<(), CompileError> {
        builder.set_id(self.get(dest)?);
        if let Some(used_by) = self.in_edges.get(&dest) {
            let mut ub = builder.init_used_by(used_by.len() as  u32);
            for (i, v) in used_by.iter().enumerate() {
                ub.set(i as u32, self.get(*v)?)
            }
        }
        Ok(())
    }

    // Assign all IDs from an iterator
    fn assign_all(&mut self, vals : &Vec<CompRef>) {
        for c in vals {
            self.map.insert(*c, self.last);
            self.last = self.last + 1;
        }
    }
}

fn build_op<S: Storage>(ids: &IDMapping, mut builder: OpBuilder<'_>,
                        op: &OpNode<'_, '_, S>, op_dest: CompRef)
                            -> Result<(), CompileError> {
    use OpNode::*;
    match op {
        Input => panic!("Should not get an input in the op builder!"),
        External(_) => panic!("Should not get an external in the op builder!"),
        &Ret(c) => {
            builder.set_ret(ids.get(c)?);
        },
        &Bind(lam, ref args) => {
            let mut b = builder.init_bind();
            ids.build_dest(op_dest, b.reborrow().init_dest())?;
            b.set_lam(ids.get(lam)?);
            let mut ab = b.init_args(args.len() as u32);
            for (i, &arg) in args.iter().enumerate() {
                ab.set(i as u32, ids.get(arg)?);
            }
        },
        &Invoke(lam) => {
            let mut b = builder.init_invoke();
            ids.build_dest(op_dest, b.reborrow().init_dest())?;
            b.set_src(ids.get(lam)?);
        },
        &Force(inv) => {
            let mut b = builder.init_force();
            ids.build_dest(op_dest, b.reborrow().init_dest())?;
            b.set_arg(ids.get(inv)?);
        },
        &Builtin(op, ref args) => {
            let mut b = builder.init_builtin();
            b.set_op(op);
            ids.build_dest(op_dest, b.reborrow().init_dest())?;
            let mut a = b.init_args(args.len() as u32);
            for (i, &v) in args.iter().enumerate() {
                a.set(i as u32, ids.get(v)?);
            }
        },
        &Match(_scrut, ref _cases) => {
            panic!("Match compilation not yet implemented")
            // let mut b = builder.init_match();
            // ids.build_dest(op_dest, b.reborrow().init_dest())?;
            // b.set_scrut(ids.get(scrut)?);
            // let mut cb = b.init_cases(cases.len() as u32);
            // for (i, c) in cases.iter().enumerate() {
            //     let b = cb.reborrow().get(i as u32);
            // }
        },
        &Select(_case, ref _branches) => {
            panic!("Select compilation not yet implemented")
        }
    }
    Ok(())
}

pub trait Compile<'s, S: Storage> {
    fn compile(&self, store: &'s S) -> Result<S::ObjectRef<'s>, CompileError>;
}

impl<'e, 's, S: Storage> Compile<'s, S> for LamGraph<'e, 's, S> {
    fn compile(&self, store: &'s S) -> Result<S::ObjectRef<'s>, CompileError> {
        // The in edges for all the reached nodes in the graph
        let mut ids = IDMapping::new();

        // Nodes in sorted order. Does not include the external ops, or inputs
        let mut ordered : Vec<CompRef> = Vec::new();
        let mut inputs : Vec<CompRef> = Vec::new();
        let mut externals : Vec<CompRef> = Vec::new();

        // The DFS traversal set, queue
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(self.get_ret().ok_or(CompileError {})?);

        // Pop from the back of the queue for DFS
        while let Some(comp) = queue.pop_back() {
            // Insert an in-edge
            if seen.insert(comp) {
                // if this is the first time we have seen this node
                let o = self.ops.get(comp).ok_or(CompileError {})?;
                // Externals and inputs
                match o.deref() {
                    OpNode::External(_) => externals.push(comp),
                    OpNode::Input => inputs.push(comp),
                    _ => ordered.push(comp)
                }
                // Insert into the in_edges map
                for c in o.children() {
                    // Register the edge
                    ids.add_dep(comp, c)
                }
                queue.extend(o.children());
            }
        }
        // Reverse the order so we are going last to first
        ordered.reverse();
        ids.assign_all(&inputs);
        ids.assign_all(&externals);
        ids.assign_all(&ordered);
        // The size of the reached set + 1 (for return)
        Ok(store.insert_build::<CompileError, _>(|build| {
            let mut cb = build.init_code();
            // set all of the inputs
            let mut ib = cb.reborrow().init_params(inputs.len() as u32);
            for (i, c) in inputs.iter().cloned().enumerate() {
                ids.build_dest(c, ib.reborrow().get(i as u32))?;
            }
            // set all of the externals
            let mut eb = cb.reborrow().init_externals(externals.len() as u32);
            for (i, c) in externals.iter().cloned().enumerate() {
                let mut ext = eb.reborrow().get(i as u32);
                // set the pointer
                ext.set_ptr(match self.ops.get(c).unwrap().deref() {
                    OpNode::External(e) => e.ptr().raw(),
                    _ => panic!("Unexpected non-externals")
                });
                ids.build_dest(c, ext.init_dest())?;
            }
            // build the code
            let mut ops = cb.init_ops(ordered.len() as u32);
            for (i, r) in ordered.iter().cloned().enumerate() {
                let op = ops.reborrow().get(i as u32);
                build_op(&ids, op, self.ops.get(r).unwrap().deref(), r)?;
            }
            Ok(())
        })?)
    }
}