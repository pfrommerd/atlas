use super::op_graph::{CodeGraph, OpNode, CompRef};
use crate::{Error, ErrorKind};
use crate::store::{ObjHandle, Storage};
use crate::store::owned::{OwnedValue, Code};
use crate::store::op::{ObjectID, DestBuilder, OpBuilder, OpAddr};
use std::collections::{VecDeque, HashMap, HashSet};
use std::ops::Deref;

struct IDMapping {
    in_edges: HashMap<CompRef, Vec<CompRef>>,
    id_map: HashMap<CompRef, ObjectID>,
    pos_map: HashMap<CompRef, OpAddr>,
    // Explicit dependencies for a given
    // computation. Used for adding a dependency
    // to the final return op
    used_by: HashMap<CompRef, Vec<OpAddr>>,
    last: ObjectID
}

impl IDMapping {
    fn new() -> Self {
        Self {
            in_edges: HashMap::new(),
            id_map: HashMap::new(),
            pos_map: HashMap::new(),
            used_by: HashMap::new(),
            last: 0
        }
    }

    fn add_in_edge(&mut self, parent: CompRef, child: CompRef) {
        self.in_edges.entry(child).or_insert(Vec::new()).push(parent);
    }

    fn add_used_by(&mut self, user: OpAddr, comp: CompRef) {
        self.used_by.entry(comp).or_insert(Vec::new()).push(user);
    }

    fn get_id(&self, c: CompRef) -> ObjectID {
        *self.id_map.get(&c).unwrap()
    }

    fn get_pos(&self, c: CompRef) -> OpAddr {
        *self.pos_map.get(&c).unwrap()
    }

    fn build_dest(&self, dest: CompRef, mut builder: DestBuilder) -> Result<(), Error> {
        builder.set_id(self.get_id(dest));
        let parents = self.in_edges.get(&dest).map_or(0, |x| x.len());
        let explicit_uses = self.used_by.get(&dest).map_or(0, |x| x.len());

        let mut ub = builder.init_used_by((parents + explicit_uses) as  u32);
        if let Some(parents) = self.in_edges.get(&dest) {
            for (i, v) in parents.iter().enumerate() {
                ub.set(i as u32, self.get_pos(*v))
            }
        }
        // Add the explicit uses
        if let Some(uses) = self.used_by.get(&dest) {
            for (i, v) in uses.iter().enumerate() {
                ub.set((parents + i) as u32, *v);
            }
        }
        Ok(())
    }

    // Assign all IDs from an iterator
    fn assign_ids(&mut self, vals : &Vec<CompRef>) {
        for c in vals {
            self.id_map.insert(*c, self.last);
            self.last = self.last + 1;
        }
    }

    fn assign_pos(&mut self, ops: &Vec<CompRef>) {
        assert!(self.pos_map.len() == 0);
        for (i, &c) in ops.iter().enumerate() {
            self.pos_map.insert(c, i as OpAddr);
        }
    }
}

fn build_op<'s, S: Storage>(alloc: &'s S, op : &OpNode<'s, S>, comp_node: CompRef,
                          ids: &IDMapping, builder: OpBuilder<'_>,
                          ready: &mut Vec<CompRef>)
                            -> Result<(), Error> {
    use OpNode::*;
    match op {
        Indirect(_) => panic!("Should not get an indirect in the op builder!"),
        &Input(i) => {
            let mut b = builder.init_set_input();
            ids.build_dest(comp_node, b.reborrow().init_dest())?;
            b.set_input(i as u32);
            ready.push(comp_node);
        },
        External(e) => {
            let mut b = builder.init_set_external();
            ids.build_dest(comp_node, b.reborrow().init_dest())?;
            b.set_ptr(e.ptr());
            ready.push(comp_node);
        },
        ExternalGraph(g) => {
            let mut b = builder.init_set_external();
            ids.build_dest(comp_node, b.reborrow().init_dest())?;
            let handle = g.pack_new(alloc)?;
            b.set_ptr(handle.ptr());
            ready.push(comp_node);
        },
        &Bind(lam, ref args) => {
            let mut b = builder.init_bind();
            ids.build_dest(comp_node, b.reborrow().init_dest())?;
            b.set_lam(ids.get_id(lam));
            let mut ab = b.init_args(args.len() as u32);
            for (i, &arg) in args.iter().enumerate() {
                ab.set(i as u32, ids.get_id(arg));
            }
        },
        &Invoke(lam) => {
            let mut b = builder.init_invoke();
            ids.build_dest(comp_node, b.reborrow().init_dest())?;
            b.set_src(ids.get_id(lam));
        },
        &Force(inv) => {
            let mut b = builder.init_force();
            ids.build_dest(comp_node, b.reborrow().init_dest())?;
            b.set_arg(ids.get_id(inv));
        },
        Builtin(op, args) => {
            let mut b = builder.init_builtin();
            b.set_op(op.as_str());
            ids.build_dest(comp_node, b.reborrow().init_dest())?;
            let mut a = b.init_args(args.len() as u32);
            for (i, &v) in args.iter().enumerate() {
                a.set(i as u32, ids.get_id(v));
            }
            if args.is_empty() {
                ready.push(comp_node)
            }
        },
        &Match(_scrut, ref _cases) => {
            panic!("Match compilation not yet implemented")
        },
    }
    Ok(())
}

pub trait Pack<'s, S: Storage> {
    fn pack_new(&self, alloc: &'s S) -> Result<ObjHandle<'s, S>, Error>;
}

impl<'s, S: Storage> Pack<'s, S> for CodeGraph<'s, S> {
    fn pack_new(&self, alloc: &'s S) -> Result<ObjHandle<'s, S>, Error> {
        // The in edges for all the reached nodes in the graph
        let mut ids = IDMapping::new();

        // Nodes in sorted order. Does not include the external ops, or inputs
        let mut ordered : Vec<CompRef> = Vec::new();
        // The DFS traversal set, queue
        let mut seen = HashSet::new();
        let mut queue = VecDeque::new();

        let output = self.get_output()
            .ok_or(Error::new_const(ErrorKind::Compile, 
                "No output specified for code graph"))?;
        queue.push_back(output);

        // Pop from the back of the queue for DFS
        while let Some(comp) = queue.pop_back() {
            // Insert an in-edge
            if seen.insert(comp) {
                // if this is the first time we have seen this node
                let o = self.get(comp)
                    .ok_or(Error::new_const(ErrorKind::Compile, 
                        "Internal graph error"))?;
                // Push the comp onto the stack
                ordered.push(comp);
                // Insert into the in_edges map
                for c in o.children() {
                    // Register the edge
                    ids.add_in_edge(comp, c)
                }
                queue.extend(o.children());
            }
        }
        // Reverse the order so we are going last DFS to first DFS
        ordered.reverse();
        ids.assign_ids(&ordered);
        ids.assign_pos(&ordered);
        // put an extra dependency from the final output to the return op
        ids.add_used_by(ordered.len() as OpAddr, output);

        let mut c = Code::new();
        let mut builder = c.builder();

        let mut ready = Vec::new();
        // build the code
        let mut ops = builder.reborrow().init_ops(ordered.len() as u32 + 1);
        for (i, comp) in ordered.iter().cloned().enumerate() {
            let op_builder = ops.reborrow().get(i as u32);
            let op = self.get(comp).unwrap();
            let op = op.deref();
            build_op(alloc, op, comp, &ids, op_builder, &mut ready)?;
        }
        // set the final return to point to the object id of the output
        let mut return_builder = ops.get(ordered.len() as u32);
        return_builder.set_ret(ids.get_id(output));

        // set the ready operations
        let mut rb = builder.init_ready(ready.len() as u32);
        for (i, comp) in ready.iter().enumerate() {
            rb.set(i as u32, ids.get_pos(*comp))
        }

        let h = OwnedValue::Code(c).pack_new(alloc)?;
        Ok(h)
    }
}