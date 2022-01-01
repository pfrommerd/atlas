use crate::core::lang::{
    ExprReader, ParamWhich, ArgWhich, PrimitiveReader, PrimitiveWhich, ExprWhich, DisambID
};
use super::op::{Program, Segment, Op, OpPrimitive, UnpackOp, ApplyOp, RegAddr};
use std::collections::{HashMap, HashSet};

pub struct CompileError {}

impl From<capnp::Error> for CompileError {
    fn from(_: capnp::Error) -> Self {
        Self {}
    }
}
impl From<capnp::NotInSchema> for CompileError {
    fn from(_: capnp::NotInSchema) -> Self {
        Self {}
    }
}

pub trait Compile<'e> {
    fn compile(&self, regs: &mut RegisterMap<'e>,
                seg: &mut Segment<'e>, prog: &mut Program<'e>) -> Result<RegAddr, CompileError>;
}
pub struct RegisterMap<'s> {
    symbols: HashMap<(&'s str, DisambID), RegAddr>,
    used: Vec<bool>, // TODO: Use Arc, AtomicCell for thread safety
}

impl<'s> RegisterMap<'s> {
    pub fn new() -> Self {
        RegisterMap {
            symbols: HashMap::new(),
            used: Vec::new(),
        }
    }
    pub fn get(&self, sym: (&str, DisambID)) -> Option<RegAddr> {
        self.symbols.get(&sym).map(|x| *x)
    }
    pub fn add(&mut self, sym: (&'s str, DisambID), addr: RegAddr) {
        self.symbols.insert(sym, addr);
    }

    pub fn req_reg(&mut self) -> RegAddr {
        for (i, b) in self.used.iter_mut().enumerate() {
            if !*b {
                *b = true;
                return i as RegAddr;
            }
        }
        let i = self.used.len() as RegAddr;
        self.used.push(true);
        return i;
    }
}

// To compile a primitive into a register, just return
impl<'e> Compile<'e> for PrimitiveReader<'e> {
    fn compile(&self, regs: &mut RegisterMap, seg: &mut Segment<'e>, _: &mut Program<'e>) 
            -> Result<RegAddr,CompileError> {
        use PrimitiveWhich::*;
        let arg = match self.which()? {
            Unit(_) => OpPrimitive::Unit,
            Bool(b) => OpPrimitive::Bool(b),
            Int(i) => OpPrimitive::Int(i),
            Float(f) => OpPrimitive::Float(f),
            Char(c) => OpPrimitive::Char(std::char::from_u32(c).ok_or(CompileError{})?),
            String(s) => OpPrimitive::String(s?),
            Buffer(b) => OpPrimitive::Buffer(b?),
            EmptyList(_) => OpPrimitive::EmptyList,
            EmptyTuple(_) => OpPrimitive::EmptyTuple,
            EmptyRecord(_) => OpPrimitive::EmptyRecord
        };
        let reg = regs.req_reg();
        seg.append(Op::Store(reg, arg));
        Ok(reg)
    }
}

fn compile_lambda<'e>(expr: &ExprReader<'e>,
            regs: &mut RegisterMap, seg: &mut Segment<'e>, prog: &mut Program<'e>) -> Result<RegAddr, CompileError> {
    let dest = regs.req_reg();
    let lam = match expr.which().unwrap() {
        ExprWhich::Lam(l) => l,
        _ => panic!("Must supply lambda")
    };

    let new_seg_id = prog.gen_id();
    let target_id = seg.add_target(new_seg_id);

    seg.append(Op::Store(dest, OpPrimitive::ExternalTarget(target_id)));

    let mut new_seg = Segment::new();
    let mut new_regs = RegisterMap::new();

    // unpack the parameters in the new segment
    let params = lam.get_params().unwrap();
    for p in params.iter() {
        let s = p.get_symbol().unwrap();
        let sym = (s.get_name().unwrap(), s.get_disam());

        let new_reg = new_regs.req_reg();
        new_regs.add(sym, new_reg);

        // unpack the parameter to the right register (or no register if not used)
        use ParamWhich::*;
        let uop = match p.which()? {
            Pos(_) => UnpackOp::Pos(new_reg),
            Named(s) => UnpackOp::Named(new_reg, s?),
            Optional(s) => UnpackOp::Optional(new_reg, s?),
            VarPos(_) => UnpackOp::VarPos(new_reg),
            VarKey(_) => UnpackOp::VarKey(new_reg)
        };
        new_seg.append(Op::Unpack(uop));
    }

    // find all of the free variables we need to lift into the lambda
    let fv = expr.free_variables(&HashSet::new());
    // if we have to lift things into the lambda,
    for sym in fv.iter() {
        // get a register for the free variable
        let new_reg = new_regs.req_reg();
        new_regs.add(*sym, new_reg); // add to the new map

        let old_reg = regs.get(*sym).ok_or(CompileError{})?;
        seg.append(Op::ScopeSet(dest, new_reg, old_reg));
    }
    // compile the lambda into the new segment
    let res_reg = lam.get_body()?.compile(&mut new_regs, &mut new_seg, prog)?;
    new_seg.append(Op::Return(res_reg));
    prog.register_seg(new_seg_id, new_seg);
    Ok(dest)
}

fn compile_apply<'e>(expr: &ExprReader<'e>, regs: &mut RegisterMap<'e>, seg: &mut Segment<'e>, prog: &mut Program<'e>) -> Result<RegAddr, CompileError> {
    let apply= match expr.which().unwrap() {
        ExprWhich::App(app) => app,
        _ => panic!("Must supply apply")
    };

    let dest = regs.req_reg();
    let mut tgt= apply.get_lam()?.compile(regs, seg, prog)?;
    let args = apply.get_args()?;
    for a in args.iter() {
        use ArgWhich::*;
        let arg = a.get_value()?.compile(regs, seg, prog)?;
        let ao = match a.which()? {
            Pos(_) => ApplyOp::Pos{dest, tgt, arg},
            ByName(name) => ApplyOp::ByName{dest, tgt, arg, name: name?},
            VarPos(_) => ApplyOp::VarPos{dest, tgt, arg},
            VarKey(_) => ApplyOp::VarKey{dest, tgt, arg}
        };
        seg.append(Op::Apply(ao));
        // change source to dest so that the next argument gets applied to dest
        tgt = dest;
    }
    Ok(dest)
}

fn compile_let<'e>(expr: &ExprReader<'e>, regs: &mut RegisterMap<'e>, seg: &mut Segment<'e>, prog: &mut Program<'e>) -> Result<RegAddr, CompileError> {
    let l = match expr.which().unwrap() {
        ExprWhich::Let(l) => l,
        _ => panic!("Must supply apply")
    };

    let binds = l.get_binds()?.get_binds()?;
    for b in binds {
        let s = b.get_symbol()?;
        let sym = (s.get_name()?, s.get_disam());
        // compile the value
        let val_reg = b.get_value()?.compile(regs, seg, prog)?;
        regs.add(sym, val_reg);
    }
    // compile the value of the let
    let res = l.get_body()?.compile(regs, seg, prog)?;
    Ok(res)
}

impl<'e> Compile<'e> for ExprReader<'e> {
    fn compile(&self, regs: &mut RegisterMap<'e>, seg: &mut Segment<'e>, prog: &mut Program<'e>) -> Result<RegAddr, CompileError> {
        use ExprWhich::*;
        let t = self.which().unwrap();
        match t {
            Id(s) => {
                let sym = s.unwrap();
                let addr = regs.get((sym.get_name().unwrap(), sym.get_disam()));
                return addr.ok_or(CompileError{})
            },
            Literal(l) => l?.compile(regs, seg, prog),
            Lam(_) => compile_lambda(self, regs, seg, prog),
            App(_) => compile_apply(self, regs, seg, prog),
            Invoke(inv) => {
                // compile the lambda entrypoint into a register
                let lam_reg = inv?.compile(regs, seg, prog)?;
                // If lam_reg is new, we can reuse it for the application
                let dest = regs.req_reg();
                // we are done with lam_reg (if reused for dest won't free)
                seg.append(Op::Invoke(dest, lam_reg));
                Ok(dest)
            },
            Let(_) => compile_let(self, regs, seg, prog),
            Match(_) => panic!("Can't compile match yet!"),
            Error(_) => panic!("Can't compile error!")
        }
    }
}