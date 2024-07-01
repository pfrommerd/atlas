pub mod parser;
pub mod syntax;

pub use crate::Constant;
pub use syntax::*;

#[derive(Clone, Hash, PartialEq, Eq, Debug)]
pub struct Type(pub String, pub Option<String>);

pub enum Node {
    Safe
}
pub struct Code {
    pub defs : Vec<u8>
}