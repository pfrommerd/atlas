pub mod graph;

use pretty::{DocAllocator,BoxAllocator, DocBuilder};


pub trait PrettyReader {
    fn pretty_doc<'b, D, A>(&self, allocator: &'b D) -> Result<DocBuilder<'b, D, A>, capnp::Error>
        where
            D: DocAllocator<'b, A>,
            D::Doc: Clone,
            A: Clone;

    fn pretty<'b, D, A>(&self, allocator: &'b D) -> DocBuilder<'b, D, A>
        where
            D: DocAllocator<'b, A>,
            D::Doc: Clone,
            A: Clone {
        match self.pretty_doc(allocator) {
            Ok(s) => s,
            Err(_) => allocator.text("Capnp Read Error")
        }
    }

    fn pretty_render(&self, width:usize) -> String {
        let mut w = Vec::new();
        self.pretty::<_, ()>(&BoxAllocator).1.render(width, &mut w).unwrap();
        String::from_utf8(w).unwrap()
    }
}

pub fn raw_slice<'s>(slice: &'s [u64]) -> &'s [u8] {
    unsafe {
        std::slice::from_raw_parts(slice.as_ptr() as *const u8,
            slice.len()*std::mem::size_of::<u64>())
    }
}

pub fn raw_mut_slice<'s>(slice: &'s mut [u64]) -> &'s mut [u8] {
    unsafe {
        std::slice::from_raw_parts_mut(slice.as_mut_ptr() as *mut u8,
            slice.len()*std::mem::size_of::<u64>())
    }
}