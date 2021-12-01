@0x9e2f84bb949c781e;

using RegAddr = UInt8;
using Target = UInt32;
using CodeHash = UInt32;

using import "value.capnp".Primitive;

struct NumericOp {
    enum OpType {
        negate @0;
        add @1;
        mul @2;
        mod @3;
        or @4;
        and @5;
    }
    type @0 :OpType;
    target @1 :RegAddr;
    src @2 :RegAddr;
    arg @3 :RegAddr;
}

struct Op {
    union {
        force @0: RegAddr;
        ret @1: RegAddr;

        store :group {
            reg @2 :RegAddr;
            val @3 :Primitive;
        }
        compute @4 :NumericOp;

        entrypoint :group {
            reg @5 :RegAddr;
            targetId @6 :Target; # entry point
        }
        push :group {
            reg @7 :RegAddr; # register of the entrypoint
            value @8 :RegAddr; # regsiter of the value
        }

        thunk :group {
            reg @9 :RegAddr;
            entrypoint @10 :RegAddr;
        }
        jmpIf :group {
            reg @11 :RegAddr;
            targetId @12 :Target;
        }
    }
}