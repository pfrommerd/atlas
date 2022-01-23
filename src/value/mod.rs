pub mod storage;
pub mod mem;
pub mod local;
pub mod allocator;

#[cfg(test)]
mod test;

pub use crate::value_capnp::value::{
    Reader as ValueReader,
    Builder as ValueBuilder,
    Which as ValueWhich
};
pub use crate::value_capnp::packed_heap::{
    Reader as PackedHeapReader
};
pub use crate::value_capnp::arg_value::{
    Which as ArgValueWhich
};
use crate::vm::ExecError;

use capnp::message::TypedReader;
use capnp::serialize::SliceSegments;
pub type ValueRootReader<'r> = TypedReader<SliceSegments<'r>, crate::value_capnp::value::Owned>;

pub use crate::value_capnp::primitive::{
    Which as PrimitiveWhich,
    Builder as PrimitiveBuilder,
    Reader as PrimitiveReader
};
pub use crate::op_capnp::param::{
    Which as ParamWhich,
    Reader as ParamReader,
    Builder as ParamBuilder
};
pub use crate::op_capnp::code::{
    Reader as CodeReader
};
pub use storage::{
    Storage, ObjPointer, ObjectRef, DataRef, StorageError
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Numeric {
    Int(i64),
    Float(f64)
}

impl Numeric {
    pub fn op(l: Numeric, r: Numeric, iop : fn(i64, i64) -> i64, fop : fn(f64, f64) -> f64) -> Numeric {
        match (l, r) {
            (Numeric::Int(l), Numeric::Int(r)) => Numeric::Int(iop(l, r)),
            (Numeric::Int(l), Numeric::Float(r)) => Numeric::Float(fop(l as f64, r)),
            (Numeric::Float(l), Numeric::Int(r)) => Numeric::Float(fop(l,r as f64)),
            (Numeric::Float(l), Numeric::Float(r)) => Numeric::Float(fop(l,r))
        }
    }
    pub fn set(self, mut builder: PrimitiveBuilder<'_>) {
        match self {
            Self::Int(i) => builder.set_int(i),
            Self::Float(f) => builder.set_float(f)
        }
    }
}

pub trait ExtractValue<'s> {
    fn thunk(&self) -> Option<ObjPointer>;
    fn code(&self) -> Option<CodeReader<'s>>;
    fn numeric(&self) -> Result<Numeric, ExecError>;
}

impl<'s> ExtractValue<'s> for ValueReader<'s> {
    fn thunk(&self) -> Option<ObjPointer> {
        match self.which().ok()? {
            ValueWhich::Thunk(t) => Some(ObjPointer::from(t)),
            _ => None
        }
    }
    fn code(&self) -> Option<CodeReader<'s>> {
        match self.which().ok()? {
            ValueWhich::Code(r) => r.ok(),
            _ => None
        }
    }

    fn numeric(&self) -> Result<Numeric, ExecError> {
        match self.which()? {
            ValueWhich::Primitive(p) => {
                match p?.which()? {
                    PrimitiveWhich::Float(f) => {
                        Ok(Numeric::Float(f))
                    },
                    PrimitiveWhich::Int(i) => {
                        Ok(Numeric::Int(i))
                    },
                    _ => Err(ExecError {})
                }
            }
            _ => Err(ExecError {})
        }
    }
}

use std::collections::HashMap;

pub trait HeapRemapable {
    fn remap_into(&self, builder: ValueBuilder<'_>, 
                map: &HashMap<u64, u64>) -> Result<(), StorageError>;
}

impl HeapRemapable for ValueReader<'_> {
    fn remap_into(&self, mut builder: ValueBuilder<'_>,
                map: &HashMap<u64, u64>) -> Result<(), StorageError> {
        use ValueWhich::*;
        match self.which()? {
        Code(r) => {
            let r = r?;
            let mut cb = builder.init_code();
            cb.set_closure(r.reborrow().get_closure()?)?;
            cb.set_ops(r.reborrow().get_ops()?)?;
            let constants = r.get_constants()?;
            let mut new_constants = cb.init_constants(constants.len());
            for (i, v) in constants.iter().enumerate() {
                new_constants.reborrow().get(i as u32).set_dest(v.get_dest()?)?;
                new_constants.reborrow().get(i as u32).set_ptr(map[&v.get_ptr()])
            }
        },
        Closure(r) => {
            let mut cb = builder.init_closure();
            cb.set_code(map[&r.get_code()]);
            let entries = r.get_entries()?;
            let mut new_entries = cb.init_entries(entries.len());
            for (i, v) in entries.iter().enumerate() {
                new_entries.set(i as u32, map[&v]);
            }
        },
        Apply(r) => {
            let mut ab = builder.init_apply();
            ab.set_lam(map[&r.get_lam()]);
            let args = r.get_args()?;
            let mut new_args = ab.init_args(args.len());
            for (i, v) in args.iter().enumerate() {
                let mut a = new_args.reborrow().get(i as u32);
                a.set_val(map[&v.get_val()]);
                use ArgValueWhich::*;
                match v.which()? {
                    Pos(_) => a.set_pos(()),
                    Key(k) => a.set_key(map[&k]),
                    VarPos(_) => a.set_var_pos(()),
                    VarKey(_) => a.set_var_key(())
                }
            }
        },
        Thunk(p) => builder.set_thunk(map[&p]),
        Primitive(p) => builder.set_primitive(p?)?,
        Record(r) => {
            let rec = r?;
            let mut rb = builder.init_record(rec.len());
            for (i, r) in rec.iter().enumerate() {
                let mut e = rb.reborrow().get(i as u32);
                e.set_key(map[&r.get_key()]);
                e.set_val(map[&r.get_val()]);
            }
        },
        Tuple(r) => {
            let tup = r?;
            let mut t = builder.init_tuple(tup.len());
            for (i, v) in tup.iter().enumerate() {
                t.set(i as u32, map[&v]);
            }
        },
        Cons(r) => {
            let mut c = builder.init_cons();
            c.set_head(map[&r.get_head()]);
            c.set_tail(map[&r.get_tail()]);
        },
        Nil(_) => builder.set_nil(()),
        Variant(r) => {
            let mut vb = builder.init_variant();
            vb.set_tag(map[&r.get_tag()]);
            vb.set_value(map[&r.get_value()]);
        },
        CoreExpr(r) => builder.set_core_expr(r?)?,
        }
        Ok(())
    }
}

pub trait UnpackHeap {
    fn unpack_into<'s, S: Storage>(&self, store: &'s S) -> Result<Vec<S::EntryRef<'s>>, StorageError>;
}

impl UnpackHeap for PackedHeapReader<'_> {
    fn unpack_into<'s, S: Storage>(&self, store: &'s S) -> Result<Vec<S::EntryRef<'s>>, StorageError> {
        let mut entries = HashMap::new();
        // remapping from original pointer to target
        let mut map  : HashMap<u64, u64> = HashMap::new();
        for e in self.get_entries()?.iter() {
            let entry = store.alloc()?;
            map.insert(e.get_loc(), entry.ptr().raw());
            entries.insert(e.get_loc(), entry);
        }
        for e in self.get_entries()?.iter() {
            let val = store.insert_value_build(|b| {
                e.get_val()?.remap_into(b, &map)
            })?;
            entries[&e.get_loc()].set_value(val);
        }
        // get the entries from the entry map
        Ok(self.get_roots()?.iter().map(|x| entries[&x].clone()).collect())
    }
}