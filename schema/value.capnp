@0xa8bf5a028fbed6bb;

struct Primitive {
    union {
        unit @0 :Void;
        bool @1 :Bool;
        int @2 :Int64;
        float @3 :Float64;
        string @4 :Text;
        char @5 :UInt32; # Unicode character
        buffer @6 :Data;
        emptyList @7 :Void;
        emptyTuple @8 :Void;
        emptyRecord @9 :Void;
    }
}

using Pointer = UInt64;
using ThunkID = UInt64;

struct RecordEntry {
    key @0 :Pointer;
    val @1 :Pointer;
}

using import "op.capnp".Code;
using import "op.capnp".ApplyType;
using import "core.capnp".Expr;

struct Value {
    union {
        primitive @0 :Primitive;
        code @1 :Code;
        coreExpr @2 :Expr;
        record @3 :List(RecordEntry);
        tuple @4 :List(Pointer);
        cons :group {
            head @5 :Pointer;
            tail @6 :Pointer;
        }
        # empty list
        nil @7 :Void;
        variant :group {
            tag @8 :Pointer;
            value @9 :Pointer;
        }
        partial :group {
            lam @10 :Pointer;
            types @11 :List(ApplyType);
            args @12 :List(Pointer);
        }
        thunk :group {
            lam @13 :Pointer;
            argTypes @14 :List(ApplyType);
            args @15 :List(Pointer);
        }
    }
}

# Tables are maps from
# CodeHash to trace