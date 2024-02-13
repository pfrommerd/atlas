pub mod local;

pub use std::io::Error;

use std::io::ErrorKind;
use std::time::SystemTime;
use uuid::Uuid;
use std::ffi::{OsStr, OsString};
use crate::util::AsyncIterator;

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


pub trait IOHandle<'fs> : std::fmt::Display + Clone + 'fs {
    async fn read(&self, file_off: u64, buf: &mut [u8]) -> Result<usize, Error>;
    async fn write(&self, file_off: u64, buf: &[u8]) -> Result<usize, Error>;
}

pub trait DirHandle<'fs> : std::fmt::Display + Clone + 'fs  {
    type File : File<'fs>;
    type Iterator : AsyncIterator<Item=Result<(FileNameBuf, Self::File), Error>>;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileType {
    Regular, Directory, Symlink
}

// A File handle
pub trait File<'fs> : Clone + 'fs {
    type IOHandle : IOHandle<'fs> + 'fs;
    type DirHandle : DirHandle<'fs, File=Self> + 'fs;

    fn id(&self) -> Id { 
        Id(self.storage_id(), self.file_id())
    }

    fn file_id(&self) -> FileId;
    // The id of the storage
    // containing this file. 
    // This can be used to determine
    // if two files are on the same underlying storage
    fn storage_id(&self) -> StorageId;

    fn file_type(&self) -> FileType;

    async fn get_attr(&self, a: Attribute) -> Result<AttrValue, Error>;
    async fn get_attr_into<T : TryFrom<AttrValue, Error=Error>>(&self, a: Attribute) -> Result<T, Error> {
        match self.get_attr(a).await {
            Ok(v) => T::try_from(v),
            Err(e) => Err(e)
        }
    }

    async fn set_attr(&self, a: Attribute, val: AttrValue) -> Result<(), Error>;

    async fn children(&self) -> Result<Self::DirHandle, Error>;

    // remove a child
    async fn remove(&self, part: &Location) -> Result<(), Error>;

    async fn put(&self, part: &Location, handle : Self) -> Result<(), Error>;
    async fn create(&self, part: FileNameBuf,
                    file_type: FileType,
                    // initial attribute sto create the file with
                    attrs: Vec<(Attribute, AttrValue)>
    ) -> Result<Self, Error>;
    async fn get(&self, part: &Location) -> Result<Option<Self>, Error>;

    async fn mount<F: FileSystem>(&self, fs: F) -> Result<(), Error>;
    async fn unmount(&self) -> Result<(), Error>;

    // actually do io on the file
    async fn open<'s>(&'s self, flags: OpenFlags) -> Result<Self::IOHandle, Error>;
}

pub trait FileSystem {
    type File<'fs> : File<'fs> where Self : 'fs;
    fn root<'fs>(&'fs self) -> Result<Self::File<'fs>, Error>;
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

impl TryFrom<AttrValue> for u16 {
    type Error = Error;
    fn try_from(v: AttrValue) -> Result<u16, Error> {
        use AttrValue::*;
        match v {
            U16(i) => Ok(i),
            _ => Err(Error::from(ErrorKind::Unsupported))
        }
    }
}

impl TryFrom<AttrValue> for u32 {
    type Error = Error;
    fn try_from(v: AttrValue) -> Result<u32, Error> {
        use AttrValue::*;
        match v {
            U32(i) => Ok(i),
            _ => Err(Error::from(ErrorKind::Unsupported))
        }
    }
}

impl TryFrom<AttrValue> for u64 {
    type Error = Error;
    fn try_from(v: AttrValue) -> Result<u64, Error> {
        use AttrValue::*;
        match v {
            U64(i) => Ok(i),
            _ => Err(Error::from(ErrorKind::Unsupported))
        }
    }
}

impl TryFrom<AttrValue> for String {
    type Error = Error;
    fn try_from(v: AttrValue) -> Result<String, Error> {
        use AttrValue::*;
        match v {
            String(s) => Ok(s),
            _ => Err(Error::from(ErrorKind::Unsupported))
        }
    }
}

impl TryFrom<AttrValue> for SystemTime {
    type Error = Error;
    fn try_from(v: AttrValue) -> Result<SystemTime, Error> {
        use AttrValue::*;
        match v {
            Time(s) => Ok(s),
            _ => Err(Error::from(ErrorKind::Unsupported))
        }
    }
}