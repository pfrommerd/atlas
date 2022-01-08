@0x9e2f84bb949c781e;

using RegAddr = UInt16;
using CodeHash = UInt64;
using OpAddr = UInt32;

using import "value.capnp".Primitive;
using import "value.capnp".Pointer;

struct OpArg {
}

# 32 bit offset into code block
struct BuiltinOp {
    dest @0 :RegAddr;
    union {
        unary @1 :OpArg;
        binary :group {
            left @2 :RegAddr;
            right @3 :RegAddr;
        }
    }
}

struct Target {
    union {
        offset @0 :OpAddr; # point to within this code block
        target @1 :UInt32; # index into external targets
    }
}

struct ParamOp {
    dest :union {
        reg @0 :RegAddr;
        skip @1 :Void;
    }
    union {
        pos @2 :Void;
        named @3 :Text;
        optional @4 :Text;
        varPos @5 :Void;
        varKey @6 :Void;
        done @7 :Void; # Will drop the remaining parameters
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
        builtin @4 :BuiltinOp;

        param @5 :ParamOp;

        entrypoint :group {
            reg @6 :RegAddr;
            targetId @7 :Target; # entry point
        }
        push :group {
            reg @8 :RegAddr; # register of the entrypoint
            value @9 :RegAddr; # register of the value
        }
        thunk :group {
            reg @10 :RegAddr;
            entrypoint @11 :RegAddr;
        }
        jmpIf :group {
            reg @12 :RegAddr;
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