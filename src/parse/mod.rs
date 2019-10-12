pub mod lexer;
pub mod ast;
pub mod slicer;

#[cfg(test)]
mod tests {
    use crate::parse::lexer::Lexer;
    use crate::grammar;

    #[test]
    fn parse_basic() {
        let lexer = Lexer::new("let a = 5");
        let parser = grammar::ModuleParser::new();

        let result = parser.parse(lexer);
        println!("{:?}", result);
    }
}
