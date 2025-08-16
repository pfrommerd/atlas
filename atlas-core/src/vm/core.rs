// Core graph types
use super::{Atom, AtomID, Link};

pub struct Dup {
    id: AtomID,
    a: Link,
    b: Link,
    src: Link
}

impl Atom for Dup {
    fn id(&self) -> AtomID { self.id }
}
