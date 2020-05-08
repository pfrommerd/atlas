pub mod lexer;
pub mod ast;
pub mod slicer;
lalrpop_mod!(pub grammar); // synthesized by LALRPOP

#[cfg(test)]
mod tests {
    use crate::parse::lexer::Lexer;
    use crate::grammar;

    #[test]
    fn parse_type_complex() {
        let lexer = Lexer::new("('a 'b foo), int, float");
        let parser = grammar::TypeParser::new();
        let result = parser.parse(lexer);
        println!("{:?}", result);
        result.unwrap();
    }

    #[test]
    fn parse_mod_typeonly() {
        let lexer = Lexer::new("type 'a 'b foo = ('a, 'b)");
        let parser = grammar::ModuleParser::new();

        let result = parser.parse(lexer);
        println!("{:?}", result);
        result.unwrap();
    }
}
