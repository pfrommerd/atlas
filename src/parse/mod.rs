#![allow(dead_code)]

pub mod ast;
pub mod lexer;
pub mod slicer;
pub mod transpile;

use std::fs;

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::parse::lexer::Lexer;
    use crate::grammar;

    #[test]
    fn parse_expr_simple() {
        let lexer = Lexer::new("1 + 2 - 3");
        let parser = grammar::ExprParser::new();
        let result = parser.parse(lexer);
        println!("{:?}", result);
        result.unwrap();
    }

    #[test]
    fn parse_mod_expronly() {
        let lexer = Lexer::new("pub let a = 1 + 1;");
        let parser = grammar::ModuleParser::new();
        let ast_expr = parser.parse(lexer);
        let transpiled = ast_expr.unwrap().transpile();
        println!("{:?}", transpiled);
    }

    #[test]
    fn transpile_prelude_ops() {
        let program = fs::read_to_string("prelude/ops.at").expect("uh oh");
        let lexer = Lexer::new(&program);
        let parser = grammar::ModuleParser::new();
        let ast_expr = parser.parse(lexer);
        let transpiled = ast_expr.unwrap().transpile();
        println!("{:?}", transpiled);
    }

    
}
