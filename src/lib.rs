#[macro_use(lalrpop_mod)] 
extern crate lalrpop_util;

#[macro_use]
extern crate gc_derive;
#[macro_use]
extern crate gc;

lalrpop_mod!(pub grammar); // synthesized by LALRPOP

pub mod parse;
pub mod core;
pub mod interp;
