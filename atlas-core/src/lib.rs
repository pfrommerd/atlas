pub mod il;
pub mod vm;

use lalrpop_util::lalrpop_mod;

lalrpop_mod!(pub il_grammar); // synthesized by LALRPOP
lalrpop_mod!(pub net_grammar); // synthesized by LALRPOP
