pub mod node;
pub mod tim;

pub use node::{
    Node, NodePtr, 
    Primitive,
    TypeNode, NodeEnv
};
pub use tim::TiMachine;