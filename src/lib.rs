#![feature(generic_associated_types)]

//use lalrpop_util::lalrpop_mod;
//lalrpop_mod!(pub grammar); // synthesized by LALRPOP

pub mod core;
//pub mod parse;
pub mod util;
pub mod value;
pub mod optim;
pub mod vm;
pub mod parse;

pub mod op_capnp {
    include!(concat!(env!("OUT_DIR"), "/op_capnp.rs"));
}

#[cfg(test)]
pub mod test_capnp {
    include!(concat!(env!("OUT_DIR"), "/test_capnp.rs"));
}