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
using import "op.capnp".Arg;
using import "core.capnp".Expr;

struct ArgValue {
    val @0 :Pointer;
    union {
        pos @1 :Void;
        # This *must* be a pointer directly to a string
        key @2 :Pointer; 
        varPos @3 :Void;
        varKey @4 :Void;
    }
}

struct Value {
    union {
        # the whnf core lambda types
        code @0 :Code;
        closure :group {
            # this pointer *must* be code
            # and cannot be a thunk
            code @1 :Pointer;
            entries @2 :List(Pointer);
        }
        apply :group {
            # note that this pointer could
            # be to another apply, code, closure
            # or even a thunk
            lam @3 :Pointer;
            args @4 :List(ArgValue);
        }
        # a pointer to the lambda
        # into which we should jump
        thunk @5 :Pointer;

        # data types
        primitive @6 :Primitive;
        record @7 :List(RecordEntry);
        tuple @8 :List(Pointer);
        cons :group {
            head @9 :Pointer;
            tail @10 :Pointer;
        }
        # empty list
        nil @11 :Void;
        variant :group {
            tag @12 :Pointer;
            value @13 :Pointer;
        }

        coreExpr @14 :Expr;
    }
}

# Tables are maps from
# CodeHash to trace