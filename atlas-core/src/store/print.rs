use super::{Handle, ReaderWhich};

use super::{StringReader, BufferReader, ObjectReader, 
            RecordReader, TupleReader, CodeReader};

use pretty::{DocAllocator, DocBuilder, Pretty, BoxAllocator, BoxDoc};
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
        String(s) => {
            let s = s.as_slice();
            a.text(format!("\"{}\"", s.deref()))
        },
        Buffer(s) => {
            let s = s.as_slice();
            a.text(format!("b\"{}\"", std::string::String::from_utf8_lossy(s.deref())))
        },
        Char(c) => a.text(format!("'{}'", c)),
        Unit => a.text("()"),
        Bot => a.text("bot"),
        Bool(b) => a.text(format!("{}", b)),
        Float(f) => a.text(format!("{}", f)),
        Int(i) => a.text(format!("{}", i)),
        Code(c) => pretty_code(&c, depth, a),
        Thunk(_) => a.text("<thunk>"),
        Record(r) => pretty_record(&r, depth, a),
        Tuple(t) => pretty_tuple(&t, depth, a),
        Partial(_) => a.text("<partial app>"),
        _ => a.text("<not printed>")
    }
}

pub fn pretty_code<'p, 's, 'a, R, D, A>(reader: &R, depth: Depth, a: &'a D) -> DocBuilder<'a, D, A>
            where R: CodeReader<'p, 's> + ?Sized, A: 'a, D: ?Sized + DocAllocator<'a, A> {
    let ops = reader.iter_ops().enumerate().map(
        |(i, op)| {
            let doc = a.text(format!("{}: ", i)).append(&op);
            let doc = if i as OpAddr == reader.get_ret() { 
                doc.append(" (ret)") 
            } else { doc };
            doc.append(a.line_())
        }
    );
    let values = reader.iter_values().enumerate().map(
        |(i, v)| {
            a.text(format!("value ${}: ", i))
                .append(a.line_()).append(pretty_handle(v.borrow(), depth, a)).append(a.line_())
        }
    );
    let ready = reader.iter_ready().map(
        |v| a.text(format!("#{}", v))
    );
    a.text("Code {").append(a.line_())
     .append(a.intersperse(values, ""))
     .append("ready: ").append(a.intersperse(ready, ", ")).append(a.line_())
     .append(a.intersperse(ops, ""))
     .append("}")
}

pub fn pretty_record<'p, 's, 'a, R, D, A>(reader: &R, depth: Depth, a: &'a D) -> DocBuilder<'a, D, A>
        where R: RecordReader<'p, 's> + ?Sized, A: 'a, D: ?Sized + DocAllocator<'a, A> {
    let pairs = reader.iter().map(
        |(k, v)| {
            pretty_handle(k.borrow(), Depth::Fixed(1), a).append(": ").append(pretty_handle(v.borrow(), depth, a))
        }
    );
    a.text("{").append(a.intersperse(pairs, ", ")).append("}")
}

pub fn pretty_tuple<'p, 's, 'a, R, D, A>(reader: &R, depth: Depth, a: &'a D) -> DocBuilder<'a, D, A>
        where R: TupleReader<'p, 's> + ?Sized, A: 'a, D: ?Sized + DocAllocator<'a, A> {
    let elems = reader.iter().map(
        |v| {
            pretty_handle(v.borrow(), depth, a)
        }
    );
    if reader.len() == 1 || reader.len() == 0 {
        a.text("(").append(a.intersperse(elems, ", ")).append(",").append(")")
    } else {
        a.text("(").append(a.intersperse(elems, ", ")).append(")")
    }
}

impl<'a, D, A> Pretty<'a, D, A> for &Dest where A: 'a, D: ?Sized + DocAllocator<'a, A> {
    fn pretty(self, a: &'a D) -> DocBuilder<'a, D, A> {
        let uses = self.uses.iter().map(|x| format!("#{}", x));
        a.text(format!("%{}", self.reg)).append("[")
            .append(a.intersperse(uses, ", ")).append("]")
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
                dest.pretty(a).append(format!(" <- force %{}", reg)),
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

use std::fmt;
use std::ops::Deref;

impl fmt::Display for Op {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let doc : BoxDoc<'_, ()> = self.pretty(&BoxAllocator).into_doc();
        write!(fmt, "{}", doc.deref().pretty(80))
    }
}