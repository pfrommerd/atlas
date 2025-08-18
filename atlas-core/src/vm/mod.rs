use std::borrow::Cow;

use serde::{Serialize, Deserialize};

pub mod ast;
pub mod builtins;

pub struct AtomType {
    domain: Cow<'static, str>,
    name: Cow<'static, str>
}

impl AtomType {
    pub const fn constant(domain: &'static str, name: &'static str) -> Self {
        AtomType {
            domain: Cow::Borrowed(domain),
            name: Cow::Borrowed(name)
        }
    }
    pub fn domain(&self) -> &str {
        self.domain.as_ref()
    }
    pub fn name(&self) -> &str {
        self.name.as_ref()
    }
}

// VActive <-> EActive bindings
// form redexes

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PortType {
    Value, Eval
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Binding {
    pub port: PortType,
    pub active: bool
}

impl Binding {
    pub fn active_value() -> Self {
        Binding { port: PortType::Value, active: true }
    }
    pub fn inactive_value() -> Self {
        Binding { port: PortType::Value, active: false }
    }
    pub fn active_eval() -> Self {
        Binding { port: PortType::Eval, active: true }
    }
    pub fn inactive_eval() -> Self {
        Binding { port: PortType::Eval, active: false }
    }
}

pub trait Wire : std::fmt::Debug + Clone {
    // Get the "other" half of the wire
    fn other(&self) -> Self;
}

pub trait AtomSerialize<S: Shard, V> {
    fn type_id(&mut self, type_id: AtomType);
    fn id(&mut self, id: S::AtomID);
    // Set the wires
    fn wires(&mut self, key: &'static str,
        wires: impl IntoIterator<Item=(Binding, S::Wire)>
    );
    //
    fn value(&mut self, value: V);
}
pub trait AtomDeserialize<S: Shard, V> {
    fn type_id(&mut self) -> AtomType;
    fn id(&mut self) -> S::AtomID;
    fn wires(&mut self, key: &'static str) -> impl Iterator<Item=(Binding, S::Wire)>;
    fn value(self) -> V;
}

pub trait Atom<S: Shard> {
    type Value : Serialize + for<'a> Deserialize<'a>;

    fn pack(self, ser: &mut impl AtomSerialize<S, Self::Value>);
    fn unpack(des: impl AtomDeserialize<S, Self::Value>) -> Self;
}

pub struct AtomPack<S: Shard, V: Serialize + for<'a> Deserialize<'a>> {
    pub type_id: AtomType,
    pub id: S::AtomID,
    pub wires: Vec<S::Wire>,
    pub value: V
}

// built-in atom types
// A "Shard" is a mutable view into a storage.
// That allows for modifications of the storage.

// An immutable storage object.
pub trait Store {
    type Handle;

    // Get a reducible expression from the queue
    fn pop_redex(&self) -> Option<impl Shard<Handle=Self::Handle>>;
}

pub trait Shard : Sized {
    type Handle;
    type Wire: Wire;

    type AtomID;

    fn lookup(&self, wire: &Self::Wire) -> Option<Self::AtomID>;

    fn create_id(&mut self) -> Self::AtomID;
    fn create_wire(&mut self) -> (Self::Wire, Self::Wire);

    fn insert(&mut self, atom: impl Atom<Self>);

    fn handle(&mut self, end: Self::Wire) -> Self::Handle;
}

pub struct Engine {

}