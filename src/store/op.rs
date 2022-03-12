use std::fmt;

use pretty::BoxAllocator;

pub type RegID = u32;
pub type ValueID = u32;
pub type InputID = u32;
pub type OpAddr = u32;
pub type OpCount = u32;

enum BuiltinOp {
    Add, Mul, Div, Exec, Read
}

struct Dest {
    reg: RegID,
    ops: Vec<OpAddr>
}

enum Op {
    Ret(RegID),
    ForceRet(RegID),
    SetValue(Dest, ValueID),
    SetInput(Dest, InputID),
    Force(Dest, RegID), // dest = src
    Bind(Dest, RegID, Vec<RegID>),
    Invoke(Dest, RegID, Vec<RegID>),
    Builtin(Dest, BuiltinOp, Vec<RegID>)
}

impl Op {
    fn num_deps(&self) -> OpCount {
        0
    }
}

/*
use pretty::{DocAllocator, DocBuilder, Pretty};

fn _pretty_code<'s, 'a, D, A>(code: &CodeReader<'s>, a: &'a D)
        -> Result<DocBuilder<'a, D, A>, capnp::Error> 
        where A: 'a, D: ?Sized + DocAllocator<'a, A>  {
    let ops = code.get_ops()?.iter().enumerate().map(
        |(i, op)| {
            a.text(format!("{}: ", i)).append(&op).append(";")
             .append(a.line_())
        }
    );
    let ready = code.get_ready()?.iter()
        .map(|val| format!("#{}", val));
    Ok(a.text("Code {").append(a.line_())
        .append("ready: ")
        .append(a.intersperse(ready, ", "))
        .append(a.line_())
        .append(a.intersperse(ops, ""))
        .append("}"))
}

impl<'s, 'a, D, A> Pretty<'a, D, A> for &CodeReader<'s> 
    where A: 'a, D: ?Sized + DocAllocator<'a, A> {

    fn pretty(self, a: &'a D) -> DocBuilder<'a, D, A> {
        match _pretty_code(self, a) {
        Ok(r) => r,
        Err(_) => {
            a.text("Capnp error")
        }
        }
    }

}

impl<'s> fmt::Display for CodeReader<'s> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut w = Vec::new();
        let doc : DocBuilder<'_, BoxAllocator, ()> = self.pretty(&BoxAllocator);
        doc.1.render(80, &mut w).unwrap();
        write!(fmt, "{}", String::from_utf8(w).unwrap())
    }
}

fn _pretty_op<'s, 'a, D, A>(op: &OpReader<'s>, a: &'a D)
            -> Result<DocBuilder<'a, D, A>, capnp::Error> 
        where A: 'a, D: ?Sized + DocAllocator<'a, A>  {
    use OpWhich::*;
    Ok(match op.which()? {
        Ret(r) => a.text("ret ").append(format!("${}", r)),
        ForceRet(r) => a.text("force_ret").append(format!("${}", r)),
        SetExternal(r) => r.get_dest()?.pretty(a)
            .append(" <- ext ").append(format!("&{}", r.get_ptr())),
        SetInput(r) => r.get_dest()?.pretty(a)
            .append(" <- input ").append(format!("{}", r.get_input())),
        Force(r) => r.get_dest()?.pretty(a)
            .append(" <- force ").append(format!("${}", r.get_arg())),
        Bind(r) => r.get_dest()?.pretty(a)
            .append(" <- bind ").append(format!("${}", r.get_lam()))
            .append(" @ ").append(a.intersperse(r.get_args()?.iter().map(|x| {
                a.text(format!("${}", x))
            }), ", ")),
        Invoke(r) => r.get_dest()?.pretty(a)
            .append(" <- invoke ").append(format!("${}", r.get_src())),
        Builtin(r) => r.get_dest()?.pretty(a)
            .append(" <- $").append(r.get_op()?.to_owned()).append("(")
            .append(a.intersperse(
                r.get_args()?.iter().map(|x| a.text(format!("${}", x))),
                ", "
            )).append(")"),
        _ => panic!()
    })
}

impl<'s, 'a, D, A> Pretty<'a, D, A> for &OpReader<'s> 
    where A: 'a, D: ?Sized + DocAllocator<'a, A> {

    fn pretty(self, a: &'a D) -> DocBuilder<'a, D, A> {
        match _pretty_op(self, a) {
        Ok(r) => r,
        Err(_) => {
            a.text("Capnp error")
        }
        }
    }

}

impl<'s> fmt::Display for OpReader<'s> {
    fn fmt(&self, fmt: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut w = Vec::new();
        let doc : DocBuilder<'_, BoxAllocator, ()> = self.pretty(&BoxAllocator);
        doc.1.render(80, &mut w).unwrap();
        write!(fmt, "{}", String::from_utf8(w).unwrap())
    }
}

impl<'s, 'a, D, A> Pretty<'a, D, A> for &DestReader<'s> 
    where A: 'a, D: ?Sized + DocAllocator<'a, A> {

    fn pretty(self, a: &'a D) -> DocBuilder<'a, D, A> {
        let id = self.get_id();
        let used_by = match self.get_used_by() {
            Ok(u) => u,
            Err(_) => return a.text("Capnp error")
        };
        let mut dest = a.text(format!("${}", id));
        if used_by.len() > 0 {
            dest = dest.append("[").append(
                a.intersperse(used_by.iter().map(
                    |x| { a.text(format!("#{}", x)) }
                ), ",")
            ).append("]");
        }
        dest
    }
}
*/