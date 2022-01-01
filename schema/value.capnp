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

        # data primtives
        emptyList @7 :Void;
        emptyTuple @8 :Void;
        emptyRecord @9 :Void;
    }
}

struct Record {

}

using import "op.capnp".Code;

using Pointer = UInt64;

struct Value {
    union {
        primitive @0 :Primitive;
        code @1 :Code;
    }
}

# Tables are maps from
# CodeHash to trace