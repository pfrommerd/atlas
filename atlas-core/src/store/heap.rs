use std::rc::Rc;
use bytes::Bytes;
use std::ops::Deref;
use std::cell::{Cell, UnsafeCell};
use crate::{Error, ErrorKind};

use super::{Storage, Handle, ObjectReader, ReaderWhich, ObjectType,
    StringReader, BufferReader, TupleReader,
    RecordReader, PartialReader, CodeReader, IndirectBuilder};


// Equivalent to value::{Value, Code}
// but uses pointers instead of handles

type Ptr = usize;


use slab::Slab;

#[derive(Default)]
pub struct HeapStorage {
    slab: UnsafeCell<Slab<Rc<Item>>>
}

impl HeapStorage {
    pub fn new() -> Self {
        Self { slab: UnsafeCell::new(Slab::new()) }
    }
    fn get<'s>(&'s self, ptr: Ptr) -> ItemHandle<'s> {
        let entry = unsafe {
            let slab = &*self.slab.get();
            slab.get(ptr - 1).cloned()
        };
        ItemHandle { store: self, ptr, entry }
    }
}

impl Storage for HeapStorage {
    type Handle<'s> = ItemHandle<'s> where Self : 's;
    type IndirectBuilder<'s> = HeapIndirectBuilder<'s> where Self : 's;

    fn indirect<'s>(&'s self) -> Result<Self::IndirectBuilder<'s>, Error> {
        let r = Rc::new(Item::Indirect(Cell::new(0)));
        let key = unsafe {
            let slab = &mut *self.slab.get();
            slab.insert(r.clone())
        };
        let handle = ItemHandle { store: self, ptr: key + 1, entry: Some(r) };
        Ok(HeapIndirectBuilder{ handle })
    }

    fn insert<'s, 'p, R>(&'s self, src: &R) -> Result<Self::Handle<'s>, Error>
                where R: ObjectReader<'p, 's, Handle=Self::Handle<'s>> { 
        use ReaderWhich::*;
        let item = match src.borrow().which() {
        Bot => Item::Bot, Unit => Item::Unit, Nil => Item::Nil,
        Indirect(h) => Item::Indirect(Cell::new(h.borrow().ptr)),
        Char(c) => Item::Char(c), Bool(b) => Item::Bool(b),
        Float(f) => Item::Float(f), Int(i) => Item::Int(i),
        String(s) => {
            let slice = s.slice(0, s.len());
            Item::String(slice.deref().to_string())
        },
        Buffer(b) => {
            let slice = b.slice(0, b.len());
            Item::Buffer(Bytes::copy_from_slice(slice.deref()))
        },
        Record(r) =>
            Item::Record(r.iter().map(|(k, v)| (k.borrow().ptr, v.borrow().ptr)).collect()),
        Tuple(t) =>
            Item::Tuple(t.iter().map(|v| v.borrow().ptr).collect()),
        Variant(k, v) =>
            Item::Variant(k.borrow().ptr, v.borrow().ptr),
        Cons(h, t) =>
            Item::Cons(h.borrow().ptr, t.borrow().ptr),
        Code(c) =>
            Item::Code(self::Code {
                ret: c.get_ret(),
                ready: c.iter_ready().collect(),
                ops: c.iter_ops().collect(),
                values: c.iter_values().map(|x| x.borrow().ptr).collect()
            }),
        Partial(p) =>
            Item::Partial(p.get_code().borrow().ptr, p.iter_args().map(|x| x.borrow().ptr).collect()),
        Thunk(p) =>
            Item::Thunk(p.borrow().ptr)
        };
        let r = Rc::new(item);
        let key = unsafe {
            let slab = &mut *self.slab.get();
            slab.insert(r.clone())
        };
        Ok(ItemHandle { store: self, ptr: key, entry: Some(r) })
    }
}

enum Item {
    Indirect(Cell<Ptr>),
    Unit,
    Bot,
    Char(char),
    Bool(bool),
    Float(f64), Int(i64),
    String(String),
    Buffer(Bytes),
    Nil, Cons(Ptr, Ptr),
    Tuple(Vec<Ptr>),
    Record(Vec<(Ptr, Ptr)>),
    Variant(Ptr, Ptr),
    Code(Code),
    Partial(Ptr, Vec<Ptr>),
    Thunk(Ptr),
}

struct Code {
    ret: OpAddr,
    ready: Vec<OpAddr>,
    ops: Vec<Op>,
    values: Vec<Ptr>
}

#[derive(Clone)]
pub struct ItemHandle<'s> {
    store: &'s HeapStorage,
    ptr: Ptr,
    entry: Option<Rc<Item>> // Will be none if a bad handle
}

impl<'s> std::hash::Hash for ItemHandle<'s> {
    fn hash<H>(&self, hasher: &mut H)
            where H: std::hash::Hasher {
        self.ptr.hash(hasher);
    }
}
impl<'s> PartialEq for ItemHandle<'s> {
    fn eq(&self, rhs: &Self) -> bool {
        self.ptr == rhs.ptr
    }
}
impl<'s> Eq for ItemHandle<'s> {}

use std::fmt;
impl<'s> fmt::Display for ItemHandle<'s> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "&{}", self.ptr)
    }
}
impl<'s> fmt::Debug for ItemHandle<'s> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(fmt, "&{}", self.ptr)
    }
}


impl<'s> Handle<'s> for ItemHandle<'s> {
    type Reader<'p> = &'p Self where Self : 'p;

    fn reader<'p>(&'p self) -> Result<Self::Reader<'p>, Error> {
        match &self.entry {
            Some(_) => Ok(self),
            None => Err(Error::new_const(ErrorKind::BadPointer, "Bad handle!"))
        }
    }
}

impl<'p, 's> ObjectReader<'p, 's> for &'p ItemHandle<'s> {
    type StringReader = StringItemReader<'p>;
    type BufferReader = BufferItemReader<'p>;
    type TupleReader = TupleItemReader<'p, 's>;
    type RecordReader = RecordItemReader<'p, 's>;
    type CodeReader = CodeItemReader<'p, 's>;
    type PartialReader = PartialItemReader<'p, 's>;

    type Handle = ItemHandle<'s>;
    type Subhandle = ItemHandle<'s>;

    fn get_type(&self) -> ObjectType {
        use ObjectType::*;
        let item = match &self.entry {
            Some(s) => s.deref(),
            None => panic!("Bad type")
        };
        match item {
            Item::Bot => Bot,
            Item::Indirect(c) => if c.get() == 0 { Bot } else { Indirect },
            Item::Unit => Unit, Item::Char(_) => Char, Item::Bool(_) => Bool,
            Item::Int(_) => Int, Item::Float(_) => Float,
            Item::String(_) => String, Item::Buffer(_) => Buffer, 
            Item::Record(_) => Record, Item::Tuple(_) => Tuple,
            Item::Variant(_, _) => Variant, Item::Cons(_, _) => Cons, Item::Nil => Nil,
            Item::Thunk(_) => Thunk, Item::Code(_) => Code, Item::Partial(_, _) => Partial
        }
    }
    fn which(&self) -> ReaderWhich<Self::Subhandle,
            Self::StringReader, Self::BufferReader,
            Self::TupleReader, Self::RecordReader,
            Self::CodeReader, Self::PartialReader> {
        use ReaderWhich::*;
        let item = match &self.entry {
            Some(s) => s.deref(),
            None => panic!("Bad type")
        };
        match item {
            Item::Bot => Bot, 
            Item::Indirect(h) => {
                let ptr = h.get();
                if ptr != 0 { Indirect(self.store.get(ptr)) } else { Bot }
            },
            Item::Unit => Unit, Item::Char(c) => Char(*c), Item::Bool(b) => Bool(*b),
            Item::Int(i) => Int(*i), Item::Float(f) => Float(*f),
            Item::String(b) => String(StringItemReader{ s: b.deref() }),
            Item::Buffer(b) => Buffer(BufferItemReader{ s: b.deref() }),
            Item::Record(record) => Record(RecordItemReader { record, store: self.store }),
            Item::Tuple(tuple) => Tuple(TupleItemReader { tuple, store: self.store }),
            Item::Variant(t, v) => Variant(self.store.get(*t), self.store.get(*v)),
            Item::Cons(h, t) => Cons(self.store.get(*h), self.store.get(*t)),
            Item::Nil => Nil,
            Item::Thunk(p) => Thunk(self.store.get(*p)),
            Item::Code(code) => Code(CodeItemReader { code, store: self.store }),
            Item::Partial(code, args) => Partial(PartialItemReader { code, args, store: self.store })
        }
    }
}

use super::op::{OpAddr, Op, ValueID};

use std::borrow::Borrow;

pub struct StringItemReader<'p> {
    s: &'p str,
}

impl<'p> StringReader<'p> for StringItemReader<'p> {
    type StringSlice<'sl> = &'sl str where Self : 'sl;

    fn slice<'sl>(&'sl self, start: usize, len: usize) -> &'sl str {
        &self.s[start..start+len]
    }
    fn len(&self) -> usize { self.s.len() }
}

pub struct BufferItemReader<'p> {
    s: &'p [u8],
}

impl<'p> BufferReader<'p> for BufferItemReader<'p> {
    type BufferSlice<'sl> = &'sl [u8] where Self : 'sl;

    fn slice<'sl>(&'sl self, start: usize, len: usize) -> &'sl [u8] {
        &self.s.borrow()[start..start+len]
    }
    fn len(&self) -> usize { self.s.len() }
}

pub struct PtrVecIter<'r, 's> {
    v: &'r Vec<Ptr>,
    store: &'s HeapStorage,
    off: usize
}

impl<'r, 's> PtrVecIter<'r, 's> {
    fn new(v: &'r Vec<Ptr>, store: &'s HeapStorage) -> Self {
        Self { v, store, off: 0 }
    }
}

impl<'r, 's> Iterator for PtrVecIter<'r, 's> {
    type Item = ItemHandle<'s>;
    fn next(&mut self) -> Option<Self::Item> {
        let res = match self.v.get(self.off) {
        Some(v) => Some(self.store.get(*v)),
        None => None
        };
        self.off = self.off + 1;
        res
    }
}

pub struct TupleItemReader<'p, 's> {
    tuple: &'p Vec<Ptr>,
    store: &'s HeapStorage
}


impl<'p,'s> TupleReader<'p, 's> for TupleItemReader<'p, 's> {
    type Subhandle = ItemHandle<'s>;
    type Handle = ItemHandle<'s>;

    type EntryIter<'r> = PtrVecIter<'r, 's> where Self : 'r;

    fn iter<'r>(&'r self) -> Self::EntryIter<'r> {
        PtrVecIter::new(self.tuple, self.store)
    }
    fn len(&self) -> usize {
        self.tuple.len()
    }
    fn get(&self, i: usize) -> Option<Self::Subhandle> {
        self.tuple.get(i).map(|x| self.store.get(*x))
    }
}

pub struct RecordItemReader<'p, 's> {
    record: &'p Vec<(Ptr, Ptr)>,
    store: &'s HeapStorage
}

pub struct RecordIter<'p, 's> {
    record: &'p Vec<(Ptr, Ptr)>,
    store: &'s HeapStorage,
    off: usize
}

impl<'p, 's> Iterator for RecordIter<'p, 's> {
    type Item = (ItemHandle<'s>, ItemHandle<'s>);
    fn next(&mut self) -> Option<Self::Item> {
        let res = match self.record.get(self.off) {
        Some((k, v)) => Some((self.store.get(*k), self.store.get(*v))),
        None => None
        };
        self.off = self.off + 1;
        res
    }
}

impl<'p, 's> RecordReader<'p, 's> for RecordItemReader<'p, 's> {
    type Handle = ItemHandle<'s>;
    type Subhandle = ItemHandle<'s>;

    type EntryIter<'r> = RecordIter<'r, 's> where Self : 'r;

    fn iter<'r>(&'r self) -> Self::EntryIter<'r> {
        RecordIter { record: self.record, store: self.store, off: 0}
    }
    fn len(&self) -> usize {
        self.record.len()
    }
    fn get(&self, i: usize) -> Option<(Self::Subhandle, Self::Subhandle)> {
        self.record.get(i).map(|(x, y)| (self.store.get(*x), self.store.get(*y)))
    }
}

pub struct PartialItemReader<'p, 's> {
    code: &'p Ptr,
    args: &'p Vec<Ptr>,
    store: &'s HeapStorage
}

impl<'p, 's> PartialReader<'p, 's> for PartialItemReader<'p, 's> {
    type Handle = ItemHandle<'s>;
    type Subhandle = ItemHandle<'s>;
    type ArgsIter<'r> = PtrVecIter<'r, 's> where Self : 'r;

    fn get_code(&self) -> Self::Subhandle {
        self.store.get(*self.code)
    }
    fn num_args(&self) -> usize {
        self.args.len()
    }
    fn get_arg(&self, i: usize) -> Option<Self::Subhandle> {
        self.args.get(i).map(|x| self.store.get(*x))
    }

    fn iter_args<'r>(&'r self) -> Self::ArgsIter<'r> {
        PtrVecIter::new(self.args, self.store)
    }
}

pub struct CodeItemReader<'p, 's> {
    code: &'p Code,
    store: &'s HeapStorage
}

impl<'p, 's> CodeReader<'p, 's> for CodeItemReader<'p, 's> {
    type Handle = ItemHandle<'s>;
    type Subhandle = ItemHandle<'s>;

    type ReadyIter<'h> = std::iter::Cloned<std::slice::Iter<'h, OpAddr>> where Self : 'h;
    type OpIter<'h> = std::iter::Cloned<std::slice::Iter<'h, Op>> where Self : 'h;
    type ValueIter<'h> = PtrVecIter<'p, 's> where Self : 'h;

    fn get_op(&self, a: OpAddr) -> Op {
        self.code.ops[a as usize].clone()
    }
    fn get_ret(&self) -> OpAddr {
        self.code.ret
    }
    fn get_value<'h>(&'h self, value_id: ValueID) -> Option<Self::Subhandle> {
        self.code.values.get(value_id as usize).map(|x| self.store.get(*x))
    }
    fn iter_ready<'h>(&'h self) -> Self::ReadyIter<'h> {
        self.code.ready.iter().cloned()
    }
    fn iter_ops<'h>(&'h self) -> Self::OpIter<'h> {
        self.code.ops.iter().cloned()
    }
    fn iter_values<'h>(&'h self) -> Self::ValueIter<'h> {
        PtrVecIter::new(&self.code.values, self.store)
    }
}

// The indirect builder

pub struct HeapIndirectBuilder<'s> {
    handle : ItemHandle<'s>
}

impl<'s> IndirectBuilder<'s> for HeapIndirectBuilder<'s> {
    type Handle = ItemHandle<'s>;
    fn handle(&self) -> ItemHandle<'s> {
        self.handle.clone()
    }

    fn build(self, dest: ItemHandle<'s>) -> ItemHandle<'s> {
        match &self.handle.entry {
            None => panic!("Bad handle"),
            Some(item) => {
                match item.deref() {
                    Item::Indirect(c) => c.set(dest.ptr),
                    _ => panic!("Bad handle contents")
                }
            }
        };
        self.handle
    }
}
