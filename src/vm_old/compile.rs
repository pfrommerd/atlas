use super::op::{CodeBuilder, Op, RegAddr, SegmentBuilder};
use crate::core::lang::{
    ArgType, Atom, Bind, Body, Cond, Expr, Literal, ParamType, Primitive, Symbol,
};
use std::collections::{HashMap, HashSet};
use std::iter::FromIterator;

pub trait Compile {
    fn compile<'p>(
        &self,
        dest: RegAddr,
        seg: &mut SegmentBuilder,
        code: &mut CodeBuilder,
        env: &RegisterMap,
    );
}

// A special trait for compiling Cond
// outputs both a symbol env and a computation result
pub trait CompileCond {
    fn compile<'p>(
        &self,
        dest: RegAddr,
        seg: &mut SegmentBuilder,
        code: &mut CodeBuilder,
        env: &'p RegisterMap,
    ) -> RegisterMap<'p>;
}

pub trait CompileEnv {
    fn compile<'p>(
        &self,
        seg: &mut SegmentBuilder,
        code: &mut CodeBuilder,
        env: &'p RegisterMap,
    ) -> RegisterMap<'p>;
}

impl Compile for Literal {
    fn compile(
        &self,
        dest: RegAddr,
        seg: &mut SegmentBuilder,
        _: &mut CodeBuilder,
        _: &RegisterMap<'_>,
    ) {
        seg.append(Op::Prim(dest, Primitive::from_literal(self.clone())));
    }
}

impl Compile for Atom {
    fn compile<'p>(
        &self,
        dest: RegAddr,
        seg: &mut SegmentBuilder,
        code: &mut CodeBuilder,
        env: &RegisterMap<'_>,
    ) {
        match self {
            Atom::Id(id) => match env.get(id) {
                Some(ptr) => {
                    seg.append(Op::Cp(dest, ptr));
                    seg.append(Op::Force(dest))
                }
                None => panic!("Missing variable {:?}", id),
            },
            Atom::Lit(lit) => lit.compile(dest, seg, code, env),
            /*
            Atom::Variant(t, a) => {
                // add a segment for the constructor
                let mut sb = code.next();
                seg.append(Op::EntrypointSeg(dest, sb.id));
                sb.append(Op::ExPosArg(0));
                sb.append(Op::Variant(1, *t, 0));
                // add the options to the variant
                for v in a {
                    sb.append(Op::VariantOpt(1, v.clone()))
                }
                sb.append(Op::Ret(1));
                code.register(sb);
            },
            */
            _ => panic!("Cannot compile atom"),
        }
    }
}

impl Compile for Body {
    fn compile<'heap>(
        &self,
        dest: RegAddr,
        seg: &mut SegmentBuilder,
        code: &mut CodeBuilder,
        env: &RegisterMap<'_>,
    ) {
        match self {
            Body::Atom(a) => a.compile(dest, seg, code, env),
            Body::Expr(e) => e.compile(dest, seg, code, env),
        }
    }
}

impl Compile for Expr {
    fn compile<'heap>(
        &self,
        dest: RegAddr,
        seg: &mut SegmentBuilder,
        code: &mut CodeBuilder,
        env: &RegisterMap<'_>,
    ) {
        use Expr::*;
        match self {
            Atom(a) => a.compile(dest, seg, code, env),
            App(l, r) => {
                l.compile(dest, seg, code, env);
                seg.append(Op::Force(dest)); // force the entrypoint
                let arg_reg = seg.next_reg();
                for (a, x) in r {
                    x.compile(arg_reg, seg, code, env);
                    match a {
                        ArgType::Pos => seg.append(Op::PosArg(dest, arg_reg)),
                        ArgType::ByName(n) => seg.append(Op::KeyArg(dest, n.clone(), arg_reg)),
                        ArgType::ExpandPos => seg.append(Op::PosVarArg(dest, arg_reg)),
                        ArgType::ExpandKeys => seg.append(Op::KeyVarArg(dest, arg_reg)),
                    }
                }
            }
            Call(l, r) => {
                l.compile(dest, seg, code, env);
                seg.append(Op::Force(dest)); // force the entrypoint
                let arg_reg = seg.next_reg();
                for (a, x) in r {
                    x.compile(arg_reg, seg, code, env);
                    match a {
                        ArgType::Pos => seg.append(Op::PosArg(dest, arg_reg)),
                        ArgType::ByName(n) => seg.append(Op::KeyArg(dest, n.clone(), arg_reg)),
                        ArgType::ExpandPos => seg.append(Op::PosVarArg(dest, arg_reg)),
                        ArgType::ExpandKeys => seg.append(Op::KeyVarArg(dest, arg_reg)),
                    }
                }
                seg.append(Op::Thunk(dest, dest));
                seg.append(Op::Force(dest))
            }
            Let(bind, body) => {
                let sub_env = bind.compile(seg, code, env);
                body.compile(dest, seg, code, &sub_env);
            }
            Lam(args, body) => {
                // lambda lifting time!
                let ignore = HashSet::from_iter(args.iter().map(|(_, s)| s.clone()));
                let free = body.free_variables(&ignore);

                let mut sub_env = RegisterMap::child(env);

                let mut sub_seg = code.next();
                seg.append(Op::EntrypointSeg(dest, sub_seg.id));

                for s in free.iter() {
                    let i = sub_seg.next_reg();
                    sub_env.set(s.clone(), i);
                    let reg = match env.get(s) {
                        None => panic!("Missing free symbol"),
                        Some(i) => i,
                    };
                    seg.append(Op::PushReg(dest, reg));
                }

                // the last arguments are the real ones
                for (p, a) in args {
                    let i = sub_seg.next_reg();
                    sub_env.set(a.clone(), i);
                    match p {
                        ParamType::Pos => sub_seg.append(Op::ExPosArg(i)),
                        ParamType::Named(s) => sub_seg.append(Op::ExNamedArg(i, s.clone())),
                        ParamType::Optional(s) => sub_seg.append(Op::ExOptNamedArg(i, s.clone())),
                        ParamType::VarPos => sub_seg.append(Op::ExPosVarArg(i)),
                        ParamType::VarKeys => sub_seg.append(Op::ExKeyVarArg(i)),
                    }
                }
                let res = sub_seg.next_reg();
                body.compile(res, &mut sub_seg, code, &sub_env);
                // values must be forced before returned because we might be returning a variable,
                // which are thunks even if they shouldn't be
                // The compiler will eliminate the thunk/this force in subsequent optimization passes
                sub_seg.append(Op::Force(res));
                sub_seg.append(Op::Ret(res));
                code.register(sub_seg);
            }
            Case(s, alts, expr) => {
                let scrut_reg = seg.next_reg();
                expr.compile(scrut_reg, seg, code, env);

                let mut nenv = RegisterMap::child(env);
                let se = match s {
                    Some(s) => {
                        nenv.set(s.clone(), scrut_reg);
                        &nenv
                    }
                    None => env,
                };
                let case_reg = seg.next_reg();
                for (c, e) in alts {
                    let senv = c.compile(case_reg, seg, code, se);
                    let mut alt_seg = code.next();
                    let res_reg = alt_seg.next_reg();
                    e.compile(res_reg, &mut alt_seg, code, &senv);
                    alt_seg.append(Op::Ret(res_reg));
                    seg.append(Op::JmpSegIf(case_reg, alt_seg.id));
                    code.register(alt_seg);
                }
            }
            Bad => panic!("Compiling bad node!"),
        }
    }
}
impl CompileCond for Cond {
    fn compile<'p>(
        &self,
        _: RegAddr,
        _: &mut SegmentBuilder,
        _: &mut CodeBuilder,
        _: &'p RegisterMap,
    ) -> RegisterMap<'p> {
        panic!("Compiling conditions not yet implemented");
    }
}

impl CompileEnv for Bind {
    fn compile<'p>(
        &self,
        seg: &mut SegmentBuilder,
        code: &mut CodeBuilder,
        env: &'p RegisterMap,
    ) -> RegisterMap<'p> {
        let mut sub_env = RegisterMap::child(env);
        match self {
            Bind::NonRec(symb, expr) => {
                let symb_reg = seg.next_reg();
                let mut sub_seg = code.next();
                seg.append(Op::EntrypointSeg(symb_reg, sub_seg.id));
                // bind everything expr needs
                let free = expr.free_variables(&HashSet::new());
                let mut nenv = RegisterMap::new();
                for var in free {
                    let reg = sub_seg.next_reg();
                    seg.append(Op::PushReg(symb_reg, env.get(&var).unwrap()));
                    nenv.set(var, reg);
                }
                // now compile the expr into the sub_seg with the sub_env
                let dest_reg = sub_seg.next_reg();
                expr.compile(dest_reg, &mut sub_seg, code, &nenv);
                sub_seg.append(Op::Ret(dest_reg));

                // thunkify the entrypoint
                seg.append(Op::Thunk(symb_reg, symb_reg));
                sub_env.set(symb.clone(), symb_reg);
                code.register(sub_seg);
            }
            Bind::Rec(bindings) => {
                let mut free_vars = Vec::new();
                let mut all_free_vars = HashSet::new();
                for (_, exp) in bindings {
                    let fv = exp.free_variables(&HashSet::new());
                    all_free_vars.extend(fv.iter().cloned());
                    free_vars.push(fv);
                }
                // figure out which ones are really recursive
                // and let's make reservations for them
                let mut res_registers = Vec::new();
                let mut rec_env = RegisterMap::child(env);
                for (s, _) in bindings {
                    if all_free_vars.contains(s) {
                        let reg = seg.next_reg();
                        seg.append(Op::Reserve(reg));
                        res_registers.push(Some(reg));
                        rec_env.set(s.clone(), reg);
                    } else {
                        res_registers.push(None)
                    }
                }
                // now we make the entrypoints
                let mut ep_registers = Vec::new();
                for ((s, expr), free) in bindings.iter().zip(free_vars.iter()) {
                    let ep_reg = seg.next_reg();
                    ep_registers.push(ep_reg);
                    let mut sub_seg = code.next();
                    seg.append(Op::EntrypointSeg(ep_reg, sub_seg.id));
                    // map in the variables we need
                    let mut nenv = RegisterMap::new();
                    for var in free {
                        let reg = sub_seg.next_reg();
                        seg.append(Op::PushReg(ep_reg, rec_env.get(&var).unwrap()));
                        nenv.set(var.clone(), reg);
                    }
                    // compile the value of the bind into the sub_env
                    let dest_reg = sub_seg.next_reg();
                    expr.compile(dest_reg, &mut sub_seg, code, &nenv);
                    sub_seg.append(Op::Ret(dest_reg));
                    // convert to a chunk
                    seg.append(Op::Thunk(ep_reg, ep_reg));
                    // let us use the resulting variable in the resulting env
                    sub_env.set(s.clone(), ep_reg);
                    // register the segment
                    code.register(sub_seg);
                }

                // Use reservations we previously made
                for (res_reg, ep_reg) in res_registers.iter().zip(ep_registers.iter()) {
                    match res_reg {
                        Some(reg) => seg.append(Op::UseReserve(*reg, *ep_reg)),
                        None => (),
                    }
                }
            }
        }
        sub_env
    }
}
pub struct RegisterMap<'p> {
    parent: Option<&'p RegisterMap<'p>>,
    symbols: HashMap<Symbol, RegAddr>,
    used: Vec<bool>,
}

impl<'p> RegisterMap<'p> {
    pub fn new() -> Self {
        RegisterMap {
            parent: None,
            symbols: HashMap::new(),
            used: Vec::new(),
        }
    }

    pub fn child(parent: &'p RegisterMap<'p>) -> Self {
        RegisterMap {
            parent: Some(parent),
            symbols: HashMap::new(),
            used: parent.used.clone(),
        }
    }

    pub fn extend(&mut self, child: HashMap<Symbol, RegAddr>) {
        self.symbols.extend(child)
    }

    pub fn set(&mut self, id: Symbol, n: RegAddr) {
        self.symbols.insert(id, n);
    }

    pub fn get(&self, id: &Symbol) -> Option<RegAddr> {
        match self.symbols.get(id) {
            Some(&ptr) => Some(ptr),
            None => match &self.parent {
                Some(parent) => parent.get(id),
                None => None,
            },
        }
    }
}
