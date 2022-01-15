@0x9e2f84bb949c781e;

using import "value.capnp".Pointer;

using ValueID = UInt16;
using OpAddr = UInt16;
using ConstantID = UInt16;
using TargetID = UInt16;

struct Dest {
    id @0 :ValueID;
    usedBy @1 :List(OpAddr);
}
struct Param {
    dest @0 :Dest;
    union {
        pos @1 :Void;
        named @2 :Text;
        optional @3 :Text;
        varPos @4 :Void;
        varKey @5 :Void;
    }
}
struct Arg {
    val @0 :ValueID;
    union {
        pos @1 :Void;
        key @2 :ValueID;
        varPos @3 :Void;
        varKey @4 :Void;
    }
}

struct Op {
    union {
        force :group {
            dest @0 :Dest;
            arg @1 :ValueID;
        }
        ret @2 :ValueID;
        recForce @3  :ValueID;
        forceRet @4  :ValueID;

        # builtins are things
        # like add, mul, div, etc
        # for now we will encode like
        # this until we know exactly
        # what we need
        builtin :group {
            dest @5 :Dest;
            op @6 :Text;
            args @7 :List(ValueID);
        }
        store :group {
            dest @8 :Dest;
            val @9 :ConstantID;
        }
        func :group {
            dest @10 :Dest;
            targetId @11 :TargetID; # entry point
            closure @12 :List(ValueID); # closure values
        }
        apply :group {
            dest @13 :Dest;
            lam @14 :ValueID;
            args @15 :List(Arg);
        }
        invoke :group {
            dest @16 :Dest;
            src @17 :ValueID;
        }
    }
}

struct Code {
    ops @0 :List(Op);
    params @1 :List(Param);
    closure @2 :List(Dest);

    # other code blocks, directly compiled in
    targets @3 :List(Pointer);

    # constants referenced in the code
    constants @4 :List(Pointer);
}