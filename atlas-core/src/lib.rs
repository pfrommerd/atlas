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

// impl std::hash::Hash for Constant {

// }


use lalrpop_util::lalrpop_mod;

lalrpop_mod!(pub il_grammar); // synthesized by LALRPOP
lalrpop_mod!(pub net_grammar); // synthesized by LALRPOP