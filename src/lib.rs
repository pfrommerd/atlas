#[macro_use(lalrpop_mod)]
extern crate lalrpop_util;

extern crate bytes;

lalrpop_mod!(pub grammar); // synthesized by LALRPOP

pub mod core;
pub mod parse;
pub mod vm;
