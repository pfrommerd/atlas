use crate::core::lang::{
    ExprReader, ParamReader, PrimitiveReader, ExprWhich, DisambID
};
use super::op::{CodeBuilder, Segment, Op, RegAddr};
use super::arena::{Arena, HeapStorage};
use std::collections::{HashMap, HashSet};

pub trait Compile<'e> {
    fn compile<H>(&self, ctx: &mut CompileContext<'e>,
                         arena: &mut Arena<H>) -> RegRef
        where H: HeapStorage;
}

pub struct CompileContext<'e> {
    pub regs: RegisterMap<'e>,
    pub seg: Segment<'e>,
}

impl<'e> CompileContext<'e> {
    pub fn new(regs: RegisterMap<'e>) -> Self {
        CompileContext {
            regs, 
            seg: Segment::new()
        }
    }

    pub fn append(&mut self, op: Op<'e>) {
        self.seg.append(op);
    }

    // build this compile context into the provided code builder
    pub fn build(&self, builder: CodeBuilder) {
    }
}

pub struct RegisterMap<'s> {
    symbols: HashMap<(&'s str, DisambID), RegAddr>,
    used: Vec<bool>, // TODO: Use Arc, AtomicCell for thread safety
}

// A reference-counting register manager
pub struct RegRef {
    pub addr: RegAddr,
    new: bool
}

impl<'s> RegisterMap<'s> {
    pub fn new() -> Self {
        RegisterMap {
            symbols: HashMap::new(),
            used: Vec::new(),
        }
    }
    pub fn get(&self, sym: (&str, DisambID)) -> Option<RegRef> {
        match self.symbols.get(&sym) {
            Some(a) => Some(RegRef{ addr: *a, new: false }),
            None => None
        }
    }
    pub fn add(&mut self, sym: (&'s str, DisambID), addr: RegAddr) {
        self.symbols.insert(sym, addr);
    }


    pub fn req_reg(&mut self) -> RegRef {
        for (i, b) in self.used.iter_mut().enumerate() {
            if !*b {
                *b = true;
                return RegRef{ addr: i as RegAddr, new: true }
            }
        }
        let i = self.used.len() as RegAddr;
        self.used.push(true);
        return RegRef{ addr: i, new: true }
    }

    pub fn try_atomic_reuse(&mut self, reg: &mut RegRef) -> RegRef {
        if reg.new {
            reg.new = false;
            RegRef { addr: reg.addr, new: false }
        } else {
            self.req_reg()
        }
    }

    pub fn done(&mut self, reg: RegRef) {
        if reg.new {
            self.used[reg.addr as usize] = false;
        }
    }
}

// To compile a primitive into a register, just return
impl<'e> Compile<'e> for PrimitiveReader<'e> {
    fn compile<H>(&self, ctx: &mut CompileContext<'e>, arena: &mut Arena<H>) -> RegRef
            where H: HeapStorage {
        let reg = ctx.regs.req_reg();
        ctx.append(Op::Store(reg.addr, self.clone()));
        reg
    }
}

fn compile_lambda<'e, H:HeapStorage>(
            expr: &ExprReader<'e>,
            ctx: &mut CompileContext<'e>,
            arena: &mut Arena<H>,
            dest: RegAddr
        ) {
    let lam = match expr.which().unwrap() {
        ExprWhich::Lam(l) => l,
        _ => panic!("Must supply lambda")
    };

    // compile the lambda body
    let mut nr = RegisterMap::new();
    // push in all of the free variables
    let fv = expr.free_variables(&HashSet::new());
    // and free variables to the registers
    let mut lifted_regs = Vec::new();
    for sym in fv.iter() {
        let addr = nr.req_reg().addr;
        nr.add(*sym, addr);
        lifted_regs.push(addr);
    }
    // find all of the parameters used in the body
    let used_params= lam.get_body().unwrap().free_variables(&fv);
    let params = lam.get_params().unwrap();
    let param_filter = |p : &ParamReader| -> bool {
        let s = p.get_symbol().unwrap();
        let sym = (s.get_name().unwrap(), s.get_disam());
        used_params.contains(&sym)
    };

    // reserve parameter registers
    let mut param_regs = Vec::new();
    for p in params.iter().filter(param_filter) {
        let s = p.get_symbol().unwrap();
        let sym = (s.get_name().unwrap(), s.get_disam());
        let addr = nr.req_reg().addr;
        nr.add(sym, addr);
        param_regs.push(addr);
    }
    // setup the sub-code block for the lambda body
    let c = CompileContext::new(nr);

    // extract registers for all of the parameters
    for p in params.iter().filter(param_filter) {
        let s= p.get_symbol().unwrap();
        let sym = (s.get_name().unwrap(), s.get_disam());
        if used_params.contains(&sym) {
        }
    }
    // compile the lambda entrypoint with the given register map

    // push registers for all of the lifted arguments into the entrypoint
    for ((name, disam), addr) in fv.iter().zip(lifted_regs.iter()) {

    }
}

impl<'e> Compile<'e> for ExprReader<'e> {
    fn compile<H>(&self, ctx: &mut CompileContext<'e>, arena: &mut Arena<H>) -> RegRef
                where H: HeapStorage {
        use ExprWhich::*;
        let t = self.which().unwrap();
        match t {
            Id(s) => {
                let sym = s.unwrap();
                let addr = ctx.regs.get((sym.get_name().unwrap(), sym.get_disam()));
                return addr.unwrap()
            },
            Lam(lam) => {
                let dest = ctx.regs.req_reg();
                dest
            },
            App(app) => {
                let dest = ctx.regs.req_reg();
                // compile the lambda entrypoint into a register
                let mut lam_reg = app.get_lam().unwrap().compile(ctx, arena);
                // If lam_reg is new, we can reuse it for the application
                let dest = ctx.regs.try_atomic_reuse(&mut lam_reg);
                // we are done with lam_reg (if reused for dest won't free)
                ctx.regs.done(lam_reg);
                dest
            },
            _ => {
                panic!("Unrecognized expr type")
            }
        }
    }
}