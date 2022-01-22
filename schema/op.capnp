@0x9e2f84bb949c781e;

using import "value.capnp".Pointer;

using ObjectID = UInt16;
using OpAddr = UInt16;
using ConstantID = UInt16;
using TargetID = UInt16;

struct Dest {
    id @0 :ObjectID;
    dependents @1 :List(OpAddr);
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
    val @0 :ObjectID;
    union {
        pos @1 :Void;
        key @2 :ObjectID;
        varPos @3 :Void;
        varKey @4 :Void;
    }
}

struct Op {
    union {
        ret @0 :ObjectID;
        # equivalent to an invoke + force + return
        # (the invoke is to ensure that the thunk is exclusively owned and we can jump directly into it)
        # the argument is the bound lambda to invoke
        tailRet @1  :ObjectID;
        force :group {
            dest @2 :Dest;
            arg @3 :ObjectID;
        }
        recForce :group {
            dest @4 :Dest;
            arg @5 :ObjectID;
        }
        closure :group {
            dest @9 :Dest;
            # the target must be a raw code pointer
            code @10 :ObjectID; 
            # closure values
            entries @11 :List(ObjectID); 
        }
        apply :group {
            dest @12 :Dest;
            lam @13 :ObjectID;
            args @14 :List(Arg);
        }
        invoke :group {
            dest @15 :Dest;
            src @16 :ObjectID;
        }
        builtin :group {
            dest @6 :Dest;
            op @7 :Text;
            args @8 :List(ObjectID);
        }
    }
}

struct Code {
    ops @0 :List(Op);
    params @1 :List(Param);
    closure @2 :List(Dest);
    # how to map constants into the ops
    constants @3 :List(Dest);
    constantVals @4 :List(Pointer);
}