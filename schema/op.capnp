@0x9e2f84bb949c781e;

using RegAddr = UInt8;
using CodeHash = UInt64;
using OpAddr = UInt32;

using import "value.capnp".Primitive;
using import "value.capnp".Pointer;

# 32 bit offset into code block
struct PrimitiveOp {
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

struct Target {
    union {
        offset @0 :OpAddr; # point to within this code block
        target @1 :UInt32; # index into external targets
    }
}

struct Op {
    union {
        force @0: RegAddr;
        ret @1: RegAddr;

        store :group {
            reg @2 :RegAddr;
            val @3 :Primitive;
        }
        compute @4 :PrimitiveOp;

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
            success @12 :Target;
            fail @13 :Target;
        }
    }
}

struct Code {
    hash @0 :CodeHash;
    label @1 :Text; # a user-friendly label for this code block, for debugging
    # Targets are jump-targets for the code
    # These are kept outside of the ops so that
    # they can easily be patched when moving objects
    # between arenas
    targets @2 :List(Pointer);
    ops @3 :List(Op);
}