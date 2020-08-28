pub mod node;
pub mod tim;

pub use node::{
    Node, NodePtr, 
    Primitive,
    BaseTypeNode, NodeEnv
};
pub use tim::TiMachine;