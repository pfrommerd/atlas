use crate::core::lang::{
    PrimitiveReader, ExprReader, DisambID
};
use super::op::{CodeBuilder, OpReader, RegAddr};
use super::arena::{Arena, Pointer, HeapStorage};
use std::collections::HashMap;

pub trait Compile {
    fn compile<H>(&self, dest: RegAddr, ctx: &mut CompileContext<'_, H>)
        where H: HeapStorage;
}

pub struct CompileContext<'c, H> 
        where H: HeapStorage {
    pub reg: RegisterMap,
    ops: &'c mut Vec<OpReader<'c>>,
    targets: &'c mut Vec<Pointer>,
    arena: &'c mut Arena<H>
}

pub struct RegisterMap {
    symbols: HashMap<(String, DisambID), RegAddr>,
    used: Vec<bool>,
}

impl RegisterMap {
    pub fn new() -> Self {
        RegisterMap {
            symbols: HashMap::new(),
            used: Vec::new(),
        }
    }
}

// Compile implementations for core lang
impl Compile for PrimitiveReader<'_> {
    fn compile<H>(&self, dest: RegAddr, ctx: &mut CompileContext<'_, H>) 
            where H: HeapStorage {
    }
}