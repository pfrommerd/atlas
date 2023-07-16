use std::io::Error;
use async_trait::async_trait;
use uuid::Uuid;

pub mod local;

// Relative file location
// (does not allow .. or /)
// basically just a sequence of path components
type Location = str;
type LocationBuf = String;

pub struct StorageId(Uuid);

// A file id
pub enum FileId {
    Uuid(Uuid), Node(u64)
}

enum Attribute {
    Size, Modified, Created, 
    Accessed, Owner, Group, Permissions,
    Named(String)
}
enum AttrValue {
    Int(i64),
    Float(f64),
    Date(u64),
    Str(String),
    PosixPerm(u32),
}

trait IOHandle {

}

type FileIO<'s> = Box<dyn IOHandle + 's>;

// A File handle
#[async_trait]
trait File<'fs> : Clone {
    fn id(&self) -> FileId;
    // The id of the storage
    // containing this file. 
    // This can be used to determine
    // if two files are on the same underlying storage
    fn storage_id(&self) -> StorageId;

    fn is_dir(&self) -> bool;

    async fn get_attr(&self, a: Attribute) -> Result<AttrValue, Error>;
    async fn set_attr(&self, a: Attribute, val: AttrValue) -> Result<(), Error>;

    async fn children<I>(&self) -> Result<I, Error>
        where I: Iterator<Item=(LocationBuf, Self)>;

    // remove a child
    async fn remove(&self, part: &Location) -> Result<(), Error>;

    async fn put(&self, part: &Location, handle : Self) -> Result<(), Error>;
    async fn create(&self, part: &Location, is_dir: bool) -> Result<Self, Error>;
    async fn get(&self, part: &Location) -> Result<Option<Self>, Error>;

    async fn mount<F: FileSystem>(&self, fs: F) -> Result<(), Error>;
    async fn unmount(&self) -> Result<(), Error>;

    // to actually do io on the file
    async fn open<'s>(&'s self) -> Result<FileIO<'s>, Error>;
}

#[async_trait]
trait FileSystem : Send {
    type FileType<'fs> : File<'fs> where Self : 'fs;
    fn root<'fs>(&'fs self) -> Result<Self::FileType<'fs>, Error>;
}

trait Sandbox {
    type FileSystem : FileSystem;

    // get the filesystem
    fn fs<'s>(&'s self) -> Result<&'s Self::FileSystem, Error>;
}