use std::any::Any;
use std::phantom::PhandomData;
use uuid::Uuid;

pub mod ast;
// pub mod core;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct Endpoint(Uuid);

pub trait Atom<'s> {
}

pub trait Wire<'s> {
    // Get the "other" side of the wire
    pub fn other(&self) -> Wire<'s>;

    // Gets the Atom + Port referred to by this wire
    pub fn get(&self) -> Endpoint;
}

// An immutable storage object.
pub trait Store {

}

// A "Shard" is a view into a storage.
// That allows for modifications of the storage.
pub trait Shard : Store + Default {
    fn wire(&'s self) -> (Wire<'s>, Wire<'s>);
}

pub trait AtomHandle<'s> {
    fn id(&self) -> AtomID;
    fn get_as<'a, A: Atom>(&'a self) -> Option<&'a A>;
}

pub struct Engine {

}
