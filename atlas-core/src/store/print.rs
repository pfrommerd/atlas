use super::{Handle, CodeReader, ObjectReader, ReaderWhich};

use pretty::{DocAllocator, DocBuilder, Pretty};
use super::op::{Op, Dest, OpAddr};
use std::borrow::Borrow;

#[derive(Clone, Copy)]
pub enum Depth {
    Fixed(usize),
    Infinite
}

impl Depth {
    pub fn is_zero(&self) -> bool {
        match self { Depth::Fixed(0) => true, _ => false }
    }
    pub fn dec(&self) -> Depth {
        match self {
            Depth::Fixed(i) => Depth::Fixed(*i - 1),
            _ => Depth::Infinite
        }
    }
}

pub fn pretty_handle<'s, 'a, H, D, A>(handle: &H, depth: Depth, a: &'a D) -> DocBuilder<'a, D, A>
            where H: Handle<'s>, A: 'a, D: ?Sized + DocAllocator<'a, A> {
    if depth.is_zero() { return a.text(format!("{}", handle)) }
    match handle.reader() {
        Ok(r) => pretty_reader(&r, depth.dec(), a),
        Err(e) => a.text(format!("<{:?}>", e))
    }
}

pub fn pretty_reader<'p, 's, 'a, R, D, A>(reader: &R, depth: Depth, a: &'a D) -> DocBuilder<'a, D, A>
            where R: ObjectReader<'p, 's>, A: 'a, D: ?Sized + DocAllocator<'a, A> {
    use ReaderWhich::*;
    match reader.which() {
        Bool(b) => a.text(format!("{}", b)),
        Float(f) => a.text(format!("{}", f)),
        Int(i) => a.text(format!("{}", i)),
        Code(c) => pretty_code(&c, depth, a),
        _ => todo!()
    }
}

pub fn pretty_code<'p, 's, 'a, R, D, A>(reader: &R, depth: Depth, a: &'a D) -> DocBuilder<'a, D, A>
            where R: CodeReader<'p, 's>, A: 'a, D: ?Sized + DocAllocator<'a, A> {
    let ops = reader.iter_ops().enumerate().map(
        |(i, op)| {
            let doc = a.text(format!("{}: ", i)).append(&op).append(a.line_());
            if i as OpAddr == reader.get_ret() { doc.append(" (ret)") } else { doc }
        }
    );
    let values = reader.iter_values().enumerate().map(
        |(i, v)| {
            let d = a.text(format!("{}: ", i)).append(pretty_handle(v.borrow(), depth, a));
            if i == 0 { a.text("value: ").append(a.line_()).append(d) } else { d }
        }
    );
    a.text("Code {")
     .append(a.intersperse(ops, ""))
     .append(a.intersperse(values, ""))
     .append("}")
}






impl<'a, D, A> Pretty<'a, D, A> for &Dest where A: 'a, D: ?Sized + DocAllocator<'a, A> {
    fn pretty(self, a: &'a D) -> DocBuilder<'a, D, A> {
        let uses = self.uses.iter().map(|x| format!("#{}", x));
        a.text(format!("%{}", self.reg)).append("[")
            .append(a.intersperse(uses, ", "))
    }
}

impl<'a, D, A> Pretty<'a, D, A> for &Op where A: 'a, D: ?Sized + DocAllocator<'a, A> {
    fn pretty(self, a: &'a D) -> DocBuilder<'a, D, A> {
        use Op::*;
        match self {
            SetValue(dest, value) =>
                dest.pretty(a).append(" <- value $").append(format!("{}", value)),
            SetInput(dest, input) =>
                dest.pretty(a).append(" <- input ").append(format!("{}", input)),
            Force(dest, reg) =>
                dest.pretty(a).append(format!(" <- %{}", reg)),
            Bind(dest, lam, args) => {
                let args = args.iter().map(|x| a.text(format!("%{}", x)));
                dest.pretty(a).append(" <- ").append(format!("%{}", lam))
                    .append(" @ ").append(a.intersperse(args, ", "))
            },
            Invoke(dest, target) =>
                dest.pretty(a).append(format!(" invoke {}%", target)),
            &Builtin(ref dest, op, ref args) => {
                let op : &'static str = op.into();
                let args = args.iter().map(|x| a.text(format!("%{}", x)));
                dest.pretty(a).append(" <- ").append(op)
                    .append(" ").append(a.intersperse(args, ", "))
            },
            Match(_, _, _) => todo!()
        }
    }
}