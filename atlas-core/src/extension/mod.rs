//! The extension API: the surface host primitives (`%name`) operate on.
//!
//! Extensions work in terms of [`Handle`] (a heap-linked, self-erasing pointer)
//! and [`Term`] (an opened node whose children are `Handle`s), rather than the
//! engine-internal [`vm::term::Term`](crate::vm::term::Term) /
//! [`vm::heap::TermPtr`](crate::vm::heap::TermPtr).

mod ext;
mod handle;
mod simple_io;
mod term;

pub use ext::{CombinedExtensions, Extensions, NoExtensions, PrimReduce};
pub use handle::{Handle, TermPtrLike};
pub use simple_io::SimpleIO;
pub use term::Term;
