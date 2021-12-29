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

using import "op.capnp".Op;
using CodeHash = UInt32;

struct Code {
    hash @0 :CodeHash;
    tag @1 :Text; # a user-friendly tag for this code block, for debugging

    # Targets are jump-targets for code
    targets @2 :List(CodeHash);
    ops @3 :List(Op);
}

struct Value {
    union {
        primitive @0 :Primitive;
        code @1 :Code;
    }
}

# Tables are maps from
# CodeHash to trace