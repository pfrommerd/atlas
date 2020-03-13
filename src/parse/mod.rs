pub mod lexer;
pub mod ast;
pub mod slicer;

#[cfg(test)]
mod tests {
    use crate::parse::lexer::Lexer;
    use crate::grammar;

    #[test]
    fn parse_basic_type() {
        let lexer = Lexer::new("type 'a 'b foo = ('a, 'b)");
        let parser = grammar::ModuleParser::new();

        let result = parser.parse(lexer);
        println!("{:?}", result);
        result.unwrap();
    }
}
