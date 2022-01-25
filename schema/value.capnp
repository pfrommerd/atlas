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
using import "core.capnp".Expr;

# struct ArgValue {
#    val @0 :Pointer;
#    union {
#        pos @1 :Void;
#        # This *must* be a pointer directly to a string
#        key @2 :Pointer; 
#        varPos @3 :Void;
#        varKey @4 :Void;
#    }
#}

struct Partial {
    code @0 :Pointer;
    args @1 :List(Pointer);
}

struct Value {
    union {
        # the whnf core lambda types
        code @0 :Code;
        partial @1 :Partial;
        # a pointer to the lambda
        # into which we should jump upon "force" being called
        thunk @2 :Pointer;
        # data types
        primitive @3 :Primitive;
        record @4 :List(RecordEntry);
        tuple @5 :List(Pointer);
        cons :group {
            head @6 :Pointer;
            tail @7 :Pointer;
        }
        # empty list
        nil @8 :Void;
        variant :group {
            tag @9 :Pointer;
            value @10 :Pointer;
        }

        coreExpr @11 :Expr;
    }
}

struct PackedHeap {
    struct HeapEntry {
        loc @0 :Pointer;
        val @1 :Value;
    }
    entries @0 :List(HeapEntry);
    roots @1 :List(Pointer);
}

# Tables are maps from
# CodeHash to trace