pub mod node;
pub mod tim;

pub use node::{
    Node, NodePtr, 
    Primitive,
    NodeEnv
};
pub use tim::TiMachine;