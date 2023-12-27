pub mod local;

pub use std::io::Error;
use std::time::SystemTime;
use uuid::Uuid;
use std::ffi::{OsStr, OsString};

use bitflags::bitflags;


// Relative file location
// (does not allow .. or leading /)
// basically just a sequence of path components
pub type Location = OsStr;
pub type LocationBuf = OsString;

#[derive(Clone, PartialOrd, Ord, 
    PartialEq, Eq, Hash)]
pub struct StorageId(Uuid);

// A file id
#[derive(Clone, PartialOrd, Ord, 
    PartialEq, Eq, Hash)]
pub enum FileId {
    Uuid(Uuid), Node(u64)
}

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
impl AttrValue {
    pub fn into_u16(self) -> Option<u16> {
        match self {
            AttrValue::U16(i) => Some(i),
            _ => None
        }
    }
    pub fn into_u32(self) -> Option<u32> {
        match self {
            AttrValue::U32(i) => Some(i),
            _ => None
        }
    }
    pub fn into_u64(self) -> Option<u64> {
        match self {
            AttrValue::U64(i) => Some(i),
            _ => None
        }
    }
    pub fn into_string(self) -> Option<String> {
        match self {
            AttrValue::String(s) => Some(s),
            _ => None
        }
    }
    pub fn into_time(self) -> Option<SystemTime> {
        match self {
            AttrValue::Time(t) => Some(t),
            _ => None
        }
    }
}

pub trait IOHandle {
    async fn read(&self, file_off: u64, buf: &mut [u8]) -> Result<usize, Error>;
}

pub trait DirHandle<'fs> {
    type FileType : File<'fs>;
    type Iterator : async_iterator::Iterator<Item=Result<(LocationBuf, Self::FileType), Error>>;

    async fn at(&self, offset : i64) -> Result<Self::Iterator, Error>;
}

bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct OpenFlags: u16 {
        const Read = 0b00000001;
        const Write = 0b00000010;
        const Append = 0b00000100;
        const ReadWrite = Self::Read.bits() | Self::Write.bits(); 
    }
}

// A File handle
pub trait File<'fs> : Clone + 'fs {
    type IOHandle : IOHandle + 'fs;
    type DirHandle : DirHandle<'fs, FileType=Self> + 'fs;

    fn id(&self) -> FileId;
    // The id of the storage
    // containing this file. 
    // This can be used to determine
    // if two files are on the same underlying storage
    fn storage_id(&self) -> StorageId;

    fn is_dir(&self) -> bool;

    async fn get_attr(&self, a: Attribute) -> Result<AttrValue, Error>;
    async fn set_attr(&self, a: Attribute, val: AttrValue) -> Result<(), Error>;

    async fn children(&self) -> Result<Self::DirHandle, Error>;

    // remove a child
    async fn remove(&self, part: &Location) -> Result<(), Error>;

    async fn put(&self, part: &Location, handle : Self) -> Result<(), Error>;
    async fn create(&self, part: &Location, is_dir: bool) -> Result<Self, Error>;
    async fn get(&self, part: &Location) -> Result<Option<Self>, Error>;

    async fn mount<F: FileSystem>(&self, fs: F) -> Result<(), Error>;
    async fn unmount(&self) -> Result<(), Error>;

    // actually do io on the file
    async fn open<'s>(&'s self, flags: OpenFlags) -> Result<Self::IOHandle, Error>;
}

pub trait FileSystem {
    type FileType<'fs> : File<'fs> where Self : 'fs;
    fn root<'fs>(&'fs self) -> Result<Self::FileType<'fs>, Error>;
}
