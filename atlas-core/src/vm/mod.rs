use std::sync::{Arc, Weak};

trait Node {

}

struct Dup {
    node: Box<dyn Node>,
    parents: Vec<Weak<DupHandle>>
}

struct DupHandle {
    dup: Arc<Dup>
}

enum Handle {
    Dup(Arc<DupHandle>),
    Node(Box<dyn Node>)
}

impl Handle {
    fn new(dup: Dup) {

    }
}
