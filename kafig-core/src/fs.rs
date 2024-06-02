use std::time::SystemTime;

use bitflags::bitflags;

type Result<T> = core::result::Result<T, std::io::Error>;

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct OpenFlags: u16 {
        const Read = 0b00000001;
        const Write = 0b00000010;
        const Append = 0b00000100;
        const ReadWrite = Self::Read.bits() | Self::Write.bits(); 
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeType {
    Regular, Directory, Symlink
}

pub enum Attribute {
    Size(u64),
    LastModified(SystemTime), LastChange(SystemTime), 
    LastAccessed(SystemTime), Created(SystemTime), 
    PosixUid(u64), PosixGid(u64), PosixPerm(u64),
    Named(String)
}

#[hermes::service]
pub trait File {
    async fn read(&self, off: u64, len: u64) -> Vec<u8>;
    async fn write(&self, off: u64, buf: Vec<u8>);
}

#[hermes::service]
pub trait Directory {
    async fn list(&self, off: u64, len: u64) -> Vec<String>;
}

#[hermes::service]
pub trait Node {
    async fn node_type(&self) -> NodeType;

    async fn size(&self) -> u64; // the size of the file
    async fn open(&self) -> FileHandle;
    async fn dir(&self) -> DirectoryHandle;

    // Opens a file or directory. If None, open the Node itself.
    async fn get(&self, path: String) -> Result<NodeHandle>;

    async fn remove(&self, path: String);
    async fn create(&self, path: String, file_type: NodeType);
}