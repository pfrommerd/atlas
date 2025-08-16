use dashmap::{DashMap, mapref::one::Ref};

#[derive(Default)]
pub struct InMemoryUniverse {
    tables : DashMap<AtomID, Box<dyn Atom>>
}

pub struct InMemoryHandle<'s>(
    Ref<'s, AtomID, Box<dyn Atom>>
);

impl<'s> Borrow<dyn Atom> for InMemoryHandle<'s> {
    fn borrow(&self) -> &dyn Atom {
        self.0.value().as_ref()
    }
}

impl<'s> AtomHandle<'s> for InMemoryHandle<'s> {
    fn id(&self) -> AtomID { *self.0.key() }
    fn get_as<'h, A: Atom>(&'h self) -> Option<&'h A> {
        let a: &dyn Any = self.0.value();
        a.downcast_ref()
    }
}

impl Universe for InMemoryUniverse {
    type Handle<'s> = InMemoryHandle<'s>;

    fn store<A: Atom>(&self, atom : A) -> InMemoryHandle {
        let id = atom.id();
        self.tables.insert(id, Box::new(atom));
        InMemoryHandle(self.tables.get(&id).unwrap())
    }
}
