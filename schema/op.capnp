@0x9e2f84bb949c781e;

using ObjectID = UInt32;
using OpAddr = UInt32;
using Pointer = UInt64;

struct Dest {
    id @0 :ObjectID;
    usedBy @1 :List(OpAddr);
}

struct Primitive {
    union {
        unit @0 :Void;
        int @1 :Int64;
        float @2 :Float64;
        bool @3 :Bool;
        char @4 :UInt32;
        string @5 :Text;
        buffer @6 :Data;
    }
}

struct Case {
    target @0 :ObjectID;
    union {
        tag @1 :Text; # tag string name
        eq @2 :Primitive;
        default @3 :Void;
    }
}

struct Op {
    union {
        ret @0 :ObjectID;
        # equivalent to a force + return
        # to prevent using a whole extra stack frame
        forceRet @1  :ObjectID;

        setExternal :group {
            dest @2 :Dest;
            ptr @3 :Pointer;
        }
        setInput :group {
            dest @4 :Dest;
            input @5 :OpAddr;
        }

        force :group {
            dest @6 :Dest;
            arg @7 :ObjectID;
        }
        bind :group {
            dest @8 :Dest;
            lam @9 :ObjectID; # must be a direct callable
            args @10 :List(ObjectID);
        }
        invoke :group {
            dest @11 :Dest;
            src @12 :ObjectID; # must be a direct callable
        }
        builtin :group {
            dest @13 :Dest;
            op @14 :Text;
            args @15 :List(ObjectID);
        }
        match :group {
            dest @16 :Dest;
            scrut @17 :ObjectID;
            cases @18 :List(Case);
        }
    }
}

struct Code {
    struct External {
        dest @0 :Dest;
        ptr @1 :Pointer;
    }
    ops @0 :List(Op);
    # A list of ops that are already ready.
    # This would be any set_external, set_input
    # or builtins without arguments
    ready @1 :List(OpAddr);
}