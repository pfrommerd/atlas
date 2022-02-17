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

        force :group {
            dest @2 :Dest;
            arg @3 :ObjectID;
        }
        bind :group {
            dest @4 :Dest;
            lam @5 :ObjectID; # must be a direct callable
            args @6 :List(ObjectID);
        }
        invoke :group {
            dest @7 :Dest;
            src @8 :ObjectID; # must be a direct callable
        }
        builtin :group {
            dest @9 :Dest;
            op @10 :Text;
            args @11 :List(ObjectID);
        }
        match :group {
            dest @12 :Dest;
            scrut @13 :ObjectID;
            cases @14 :List(Case);
        }
    }
}

struct Code {
    struct External {
        dest @0 :Dest;
        ptr @1 :Pointer;
    }
    ops @0 :List(Op);
    params @1 :List(Dest);
    externals @2 :List(External);
}