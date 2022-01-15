extern crate lalrpop;
extern crate capnpc;

fn main() {
    lalrpop::process_root().unwrap();

    capnpc::CompilerCommand::new()
        .src_prefix("schema")
        .file("schema/op.capnp")
        .file("schema/core.capnp")
        .file("schema/value.capnp")
        .file("schema/test.capnp")
        .run().expect("schema compiler command");
}