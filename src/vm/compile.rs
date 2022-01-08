use super::op::CodeBuilder;

use crate::optim::graph::{
    Graph, GraphCollection,
    GraphPtr, Node, NodePtr
};

trait Compile {
    fn compile(builder: CodeBuilder);
}

impl<'e> Compile for GraphCollection<'e> {
    fn compile(builder: CodeBuilder) {

    }
}