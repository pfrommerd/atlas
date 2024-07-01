pub mod il;
pub mod vm;

use ordered_float::OrderedFloat;


#[derive(Debug,Hash,Eq,PartialEq,Clone)]
pub enum Constant {
    Integer(i64),
    Float(OrderedFloat<f64>),
    Bool(bool),
    String(String),
    Unit
}

impl std::fmt::Display for Constant {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Constant::Integer(i) => write!(f, "{}", i),
            Constant::Float(fl) => write!(f, "{}", fl),
            Constant::Bool(b) => write!(f, "{}", b),
            Constant::String(s) => write!(f, "{}", s),
            Constant::Unit => write!(f, "()")
        }
    }
}

// impl std::hash::Hash for Constant {

// }


use lalrpop_util::lalrpop_mod;

lalrpop_mod!(pub il_grammar); // synthesized by LALRPOP
lalrpop_mod!(pub net_grammar); // synthesized by LALRPOP