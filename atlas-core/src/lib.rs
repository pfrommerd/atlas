#![feature(generic_associated_types)]

use lalrpop_util::lalrpop_mod;
lalrpop_mod!(pub grammar); // synthesized by LALRPOP

pub mod core;
pub mod util;
pub mod store;
pub mod compile;
pub mod vm;
pub mod parse;

pub use util::error::{Error, ErrorKind};