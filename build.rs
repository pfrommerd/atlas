fn main() {
    lalrpop::process_root().unwrap();

    capnpc::CompilerCommand::new()
        .src_prefix("schema")
        .file("schema/op.capnp")
        .run().expect("schema compiler command");
}