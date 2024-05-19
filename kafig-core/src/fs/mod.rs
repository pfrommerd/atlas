// pub mod local;

use crate::{Error, Result};

use std::io::ErrorKind;
use std::time::SystemTime;
use uuid::Uuid;
use std::ffi::{OsStr, OsString};
use dyn_clone::{DynClone, clone_trait_object};

use bitflags::bitflags;

// Relative file location
// (does not allow .. or leading /)
// basically just a sequence of path components
pub type Location = OsStr;
pub type LocationBuf = OsString;
pub type FileName = OsStr;
pub type FileNameBuf = OsString;

#[derive(Clone, PartialOrd, Ord, 
    PartialEq, Eq, Hash, Debug)]
pub struct StorageId(Uuid);

// A file id
#[derive(Clone, PartialOrd, Ord, 
    PartialEq, Eq, Hash, Debug)]
pub enum FileId {
    Uuid(Uuid), Node(u64)
}

#[derive(Clone, PartialOrd, Ord, 
    PartialEq, Eq, Hash, Debug)]
pub struct Id(StorageId, FileId);


#[async_trait::async_trait]
pub trait File : DynClone {
    async fn read(&self, file_off: u64, buf: &mut [u8]) -> Result<usize>;
    async fn write(&self, file_off: u64, buf: &[u8]) -> Result<usize>;
}
type DynFile = Box<dyn File>;
clone_trait_object!(File);

#[async_trait::async_trait]
pub trait Directory : DynClone {
    async fn at(&self, offset : u64, len: u64) -> Result<Vec<(FileNameBuf, DynNode)>>;

    // remove a child
    async fn get(&self, part: &Location) -> Result<Option<DynNode>>;
    async fn remove(&self, part: &Location) -> Result<()>;
    async fn create(&self, part: FileNameBuf,
                    file_type: FileType,
                    // initial attribute sto create the file with
                    attrs: Vec<(Attribute, AttrValue)>
    ) -> Result<DynNode>;
}
type DynDirectory = Box<dyn Directory>;
clone_trait_object!(Directory);

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
pub enum FileType {
    Regular, Directory, Symlink
}

// A node
#[async_trait::async_trait]
pub trait Node : DynClone {
    async fn get_attr(&self, a: Attribute) -> Result<AttrValue>;
    async fn set_attr(&self, a: Attribute, val: AttrValue) -> Result<()>;

    async fn mount(&self, fs: DynFileSystem) -> Result<()>;
    async fn unmount(&self) -> Result<()>;
    // // open a directory
    async fn opendir(&self) -> Result<DynDirectory>;
    async fn open(&self, flags: OpenFlags) -> Result<DynFile>;
    // // actually do io on the file
}
type DynNode = Box<dyn Node>;
clone_trait_object!(Node);

pub trait FileSystem {
    fn root(&self) -> DynNode;
}
type DynFileSystem = Box<dyn Node>;

pub enum Attribute {
    Size,
    LastModified, LastChange, 
    LastAccessed, Created, 
    PosixUid, PosixGid, PosixPerm,
    Named(String)
}

pub enum AttrValue {
    None,
    U16(u16),
    U32(u32),
    U64(u64),
    Time(SystemTime),
    String(String),
}

impl TryFrom<AttrValue> for u16 {
    type Error = Error;
    fn try_from(v: AttrValue) -> Result<u16> {
        use AttrValue::*;
        match v {
            U16(i) => Ok(i),
            _ => Err(Error::from(ErrorKind::Unsupported))
        }
    }
}

impl TryFrom<AttrValue> for u32 {
    type Error = Error;
    fn try_from(v: AttrValue) -> Result<u32> {
        use AttrValue::*;
        match v {
            U32(i) => Ok(i),
            _ => Err(Error::from(ErrorKind::Unsupported))
        }
    }
}

impl TryFrom<AttrValue> for u64 {
    type Error = Error;
    fn try_from(v: AttrValue) -> Result<u64> {
        use AttrValue::*;
        match v {
            U64(i) => Ok(i),
            _ => Err(Error::from(ErrorKind::Unsupported))
        }
    }
}

impl TryFrom<AttrValue> for String {
    type Error = Error;
    fn try_from(v: AttrValue) -> Result<String> {
        use AttrValue::*;
        match v {
            String(s) => Ok(s),
            _ => Err(Error::from(ErrorKind::Unsupported))
        }
    }
}

impl TryFrom<AttrValue> for SystemTime {
    type Error = Error;
    fn try_from(v: AttrValue) -> Result<SystemTime> {
        use AttrValue::*;
        match v {
            Time(s) => Ok(s),
            _ => Err(Error::from(ErrorKind::Unsupported))
        }
    }
}