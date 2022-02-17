pub use crate::op_capnp::op::{
    Which as OpWhich,
    Reader as OpReader,
    Builder as OpBuilder
};
pub use crate::op_capnp::op::force::{
    Reader as ForceReader,
    Builder as ForceBuilder
};
pub use crate::op_capnp::code::{
    Reader as CodeReader,
    Builder as CodeBuilder
};
pub use crate::op_capnp::op::match_::{
    Reader as MatchReader,
    Builder as MatchBuilder
};
pub use crate::op_capnp::dest::{
    Reader as DestReader,
    Builder as DestBuilder
};

use std::fmt;

use pretty::BoxAllocator;

pub type ObjectID = u32;
pub type OpAddr = u32;
pub type OpCount = u32;

pub trait Dependent {
    fn num_deps(&self) -> Result<OpCount, capnp::Error>;
}

impl<'s> Dependent for OpReader<'s> {
    fn num_deps(&self) -> Result<OpCount, capnp::Error> {
        use OpWhich::*;
        Ok(match self.which()? {
        Ret(_) => 1,
        ForceRet(_) => 1,
        Force(_) => 1,
        Bind(r) => r.get_args()?.len() as u32 + 1,
        Invoke(_) => 1,
        Builtin(r) => r.get_args()?.len() as u32,
        Match(c) => {
            1 + c.get_cases()?.len() as u32 + 1
        }
        })
    }
}

use pretty::{DocAllocator, DocBuilder, Pretty};

fn _pretty_code<'s, 'a, D, A>(code: &CodeReader<'s>, a: &'a D)
        -> Result<DocBuilder<'a, D, A>, capnp::Error> 
        where A: 'a, D: ?Sized + DocAllocator<'a, A>  {
    let params = code.get_params()?;
    let externals = code.get_externals()?;
    let ops = code.get_ops()?;
    Ok(a.text("Code {").append(a.line_())
    .append(a.intersperse(
        params.iter().enumerate().map(|(i, x)| {
            x.pretty(a).append(" <- input ").append(format!("{}", i))
                .append(a.line())
        }), ""))
    .append(a.intersperse(
        externals.iter().map(|x| {
                x.get_dest().unwrap().pretty(a).append(" <- ext ")
                    .append(format!("&{}", x.get_ptr())).append(a.line())
        }),""))
    .append(a.intersperse(
        ops.iter().enumerate().map(|(i, x)| {
            a.text(format!("{}: ", i)).append(x.pretty(a)).append(a.line())
        }),
    ""
    ))
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