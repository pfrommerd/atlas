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
        RecForce(_) => 1,
        Bind(r) => r.get_args()?.len() as u32 + 1,
        Invoke(_) => 1,
        Builtin(r) => r.get_args()?.len() as u32,
        Match(_) => 1,
        Select(r) => r.get_branches()?.len() as u32 + 1, 
        })
    }
}

// The pretty printers for the code
use crate::util::PrettyReader;
use pretty::{DocAllocator, DocBuilder};

impl PrettyReader for CodeReader<'_> {
    fn pretty_doc<'b, D, A>(&self, a: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
        where
            D: DocAllocator<'b, A>,
            D::Doc: Clone,
            A: Clone {
        let params = self.get_params()?;
        let externals = self.get_externals()?;
        let ops = self.get_ops()?;
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
                .append("}")
        )
    }
}

impl PrettyReader for DestReader<'_> {
    fn pretty_doc<'b, D, A>(&self, a: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
            where
                D: DocAllocator<'b, A>,
                D::Doc: Clone,
                A: Clone {
        let mut dest = a.text(format!("${}", self.get_id()));
        if self.get_used_by()?.len() > 0 {
            dest = dest.append("[").append(a.intersperse(self.get_used_by()?.iter().map(
                |x| { a.text(format!("#{}", x)) }), ",")).append("]");
        }
        Ok(dest)
    }
}

impl PrettyReader for OpReader<'_> {
    fn pretty_doc<'b, D, A>(&self, a: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
            where
                D: DocAllocator<'b, A>,
                D::Doc: Clone,
                A: Clone {
        use OpWhich::*;
        Ok(match self.which()? {
            Ret(r) => a.text("ret ").append(format!("${}", r)),
            ForceRet(r) => a.text("force_ret").append(format!("${}", r)),
            Force(r) => r.get_dest()?.pretty(a)
                .append(" <- force ").append(format!("${}", r.get_arg())),
            RecForce(r) => r.get_dest()?.pretty(a)
                .append(" <- rec_force ").append(format!("${}", r.get_arg())),
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
}