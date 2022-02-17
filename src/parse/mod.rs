pub mod ast;
pub mod lexer;
pub mod slicer;
pub mod transpile;

#[cfg(test)]
mod tests {
    use crate::grammar;
    use crate::parse::lexer::Lexer;

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
        let lexer = Lexer::new("let a = 1 + 1");
        let parser = grammar::ModuleParser::new();
        let result = parser.parse(lexer);
        println!("{:?}", result);
        result.unwrap();
    }
}
