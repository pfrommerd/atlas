#![feature(generic_associated_types)]

use lalrpop_util::lalrpop_mod;
lalrpop_mod!(pub grammar); // synthesized by LALRPOP

pub mod core;
pub mod util;
pub mod value;
pub mod optim;
pub mod vm;
pub mod parse;
pub mod error;

pub use error::{Error, ErrorKind};

pub mod op_capnp {
    include!(concat!(env!("OUT_DIR"), "/op_capnp.rs"));
}