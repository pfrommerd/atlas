#[macro_use(lalrpop_mod)] 
extern crate lalrpop_util;

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod slicer;

#[cfg(test)]
mod tests {

    #[test]
    fn parse_basic() {
    }
}

