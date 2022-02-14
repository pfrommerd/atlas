@0x9e2f84bb949c781e;

using ObjectID = UInt32;
using OpAddr = UInt32;
using Pointer = UInt64;

struct Dest {
    id @0 :ObjectID;
    usedBy @1 :List(OpAddr);
}

struct Case {
    union {
        tag @0 :Text; # tag string name
        eq @1 :ObjectID;
        default @2 :Void;
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
        recForce :group {
            dest @4 :Dest;
            arg @5 :ObjectID;
        }
        bind :group {
            dest @6 :Dest;
            lam @7 :ObjectID; # must be a direct callable
            args @8 :List(ObjectID);
        }
        invoke :group {
            dest @9 :Dest;
            src @10 :ObjectID; # must be a direct callable
        }
        builtin :group {
            dest @11 :Dest;
            op @12 :Text;
            args @13 :List(ObjectID);
        }
        match :group {
            dest @14 :Dest;
            scrut @15 :ObjectID;
            cases @16 :List(Case);
        }
        # Takes a branch number as the case
        # and will force + return the appropriate branch
        select :group {
            dest @17 :Dest;
            case @18 :ObjectID;
            branches @19 :List(ObjectID);
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