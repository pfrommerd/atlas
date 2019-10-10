#[macro_use(lalrpop_mod)] 
extern crate lalrpop_util;

pub mod ast;
pub mod lexer;

#[cfg(test)]
mod tests {
    use crate::lexer::grammar;

    #[test]
    fn parse_basic() {
        assert!(grammar::TermParser::new().parse("22").unwrap() == 22)
    }
}

