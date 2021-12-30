#[macro_use(lalrpop_mod)]
extern crate lalrpop_util;

extern crate bytes;

lalrpop_mod!(pub grammar); // synthesized by LALRPOP

pub mod core;
pub mod parse;
pub mod vm;

pub mod core_capnp {
    include!(concat!(env!("OUT_DIR"), "/core_capnp.rs"));
}
pub mod op_capnp {
    include!(concat!(env!("OUT_DIR"), "/op_capnp.rs"));
}
pub mod value_capnp {
    include!(concat!(env!("OUT_DIR"), "/value_capnp.rs"));
}