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
    }
}

struct Record {

}

using import "op.capnp".Code;

using Pointer = UInt64;

struct TableEntry {
    union {
        primitive @0 :Primitive;
        code @1 :Code;
    }
}

struct Value {
    union {
        primitive @0 :Primitive;
        code @1 :Code;
        redirect @2 :Pointer;
        tableEntry @3 :Pointer;
    }
}

# Tables are maps from
# CodeHash to trace