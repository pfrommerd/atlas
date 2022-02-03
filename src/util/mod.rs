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