use super::op::{Op, SegmentBuilder, CodeBuilder, PrimitiveOp};
use super::compile::RegisterMap;
use crate::core::lang::Symbol;

pub fn prelude(seg: &mut SegmentBuilder, code: &mut CodeBuilder) -> RegisterMap<'static> {
    let mut env = RegisterMap::new();

    let mut sb = code.next();
    env.set(Symbol::new(String::from("+"), 0), 0);
    seg.append(Op::EntrypointSeg(0, sb.id));
    sb.append(Op::ExPosArg(0));
    sb.append(Op::ExPosArg(1));
    sb.append(Op::PrimitiveOp(PrimitiveOp::Add(2, 0, 1)));
    sb.append(Op::Ret(2));

    code.register(sb);

    env
}