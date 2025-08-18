use super::*;

pub const REP_TYPE: AtomType = AtomType::constant("builtin", "replicated");
pub const DUP_TYPE: AtomType = AtomType::constant("builtin", "duplicate");
pub const ERA_TYPE: AtomType = AtomType::constant("builtin", "erase");


pub struct Replicated<S: Shard> {
    id: S::AtomID,
    replicates: Vec<S::Wire>,
    src: S::Wire,
}

impl<S: Shard> Atom<S> for Replicated<S> {
    type Value = ();

    fn pack(self, ser: &mut impl AtomSerialize<S, Self::Value>) {
        ser.type_id(REP_TYPE);
        ser.id(self.id);
        ser.wires("src", [(Binding::inactive_eval(), self.src)]);
        ser.wires("replicates", std::iter::repeat(Binding::active_value()).zip(
            self.replicates
        ));
        ser.value(());
    }

    fn unpack(mut des: impl AtomDeserialize<S, Self::Value>) -> Self {
        let id = des.id();
        let src = des.wires("src").next().expect(
            "Expected at least one wire for Replicated"
        ).1;
        let replicates = des.wires("replicates").map(|(_, w)| w).collect();
        Replicated {
            id, replicates, src
        }
    }
}

// The *active* version of replicated

pub struct Duplicate<S: Shard> {
    id: S::AtomID,
    replicates: Vec<S::Wire>,
    src: S::Wire,
}

impl<S: Shard> Atom<S> for Duplicate<S> {
    type Value = ();

    fn pack(self, ser: &mut impl AtomSerialize<S, Self::Value>) {
        ser.type_id(REP_TYPE);
        ser.id(self.id);
        ser.wires("src", [(Binding::active_eval(), self.src)]);
        ser.wires("replicates", std::iter::repeat(Binding::inactive_value()).zip(
            self.replicates
        ));
        ser.value(());
    }

    fn unpack(mut des: impl AtomDeserialize<S, Self::Value>) -> Self {
        let id = des.id();
        let src = des.wires("src").next().expect(
            "Expected at least one wire for Replicated"
        ).1;
        let replicates = des.wires("replicates").map(|(_, w)| w).collect();
        Duplicate {
            id, replicates, src
        }
    }
}


//

pub struct Erase<S: Shard> {
    id: S::AtomID,
    target: S::Wire
}

impl<S: Shard> Atom<S> for Erase<S> {
    type Value = ();

    fn pack(self, ser: &mut impl AtomSerialize<S, Self::Value>) {
        ser.type_id(ERA_TYPE);
        ser.id(self.id);
        ser.wires("target", [(Binding::active_value(), self.target)]);
        ser.value(());
    }

    fn unpack(mut des: impl AtomDeserialize<S, Self::Value>) -> Self {
        let id = des.id();
        let target = des.wires("target").next().expect(
            "Expected one wire for Erase"
        ).1;
        Erase {
            id,
            target
        }
    }
}