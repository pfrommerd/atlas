@0x9e2f84bb949c781e;

using import "value.capnp".Pointer;

using RegAddr = UInt16;
using OpAddr = UInt32;
using ConstantID = UInt32;
using TargetID = UInt32;

enum ApplyType {
    lift @0;
    pos @1;
    key @2;
    varPos @3;
    varKey @4;
}

struct Param {
    skip @0 :Bool;
    union {
        lift @1 :Void;
        pos @2 :Void;
    }
}

struct Op {
    union {
        force @0 :RegAddr;
        ret @1 :RegAddr;
        # for tail-call recursion
        retForce @2  :RegAddr;

        # builtins are things
        # like add, mul, div, etc
        # for now we will encode like
        # this until we know exactly
        # what we need
        builtin :group {
            dest @3 :RegAddr;
            op @4 :Text;
            args @5 :List(RegAddr);
        }
        trap :group {
            dest @6 :RegAddr;
            op @7 :Text;
            args @8 :List(RegAddr);
        }
        store :group {
            dest @9 :RegAddr;
            val @10 :ConstantID;
        }
        func :group {
            reg @11 :RegAddr;
            targetId @12 :TargetID; # entry point
        }
        apply :group {
            dest @13 :RegAddr;
            src @14 :RegAddr;
            arg @15 :RegAddr;
            type @16 :ApplyType;
            key @17 :RegAddr; # only used for key applications, otherwise ignored
        }
        invoke :group {
            dest @18 :RegAddr;
            src @19 :RegAddr;
        }
        # TODO: jmp operation. It is not clear how this should be handled
        # at the moment. We will wait until the rest of everything
        # is done to handle matching/jmp
    }
}

struct Code {
    ops @0 :List(Op);
    params @1 :List(Param);
    targets @2 :List(Pointer);
    constants @3 :List(Pointer);
}