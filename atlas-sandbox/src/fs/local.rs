
use super::{
    FileSystem, File, FileType,
    FileId, StorageId, Error,
    FileName, FileNameBuf,
    IOHandle, DirHandle, OpenFlags,
    Attribute, AttrValue
};
use crate::util::AsyncIterator;
use std::sync::{Arc, RwLock};
use positioned_io::{ReadAt, WriteAt};
use uuid::Uuid;
use std::path::PathBuf;
use std::fs;
use std::os::unix::fs::{PermissionsExt, MetadataExt};
use std::io::ErrorKind;


pub struct LocalFS {
    root: PathBuf,
}
impl LocalFS {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[derive(Clone)]
pub struct LocalIOHandle {
    file: Arc<RwLock<fs::File>>,
}

impl std::fmt::Display for LocalIOHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LocalIOHandle")
    }
}

impl IOHandle<'_> for LocalIOHandle {
    async fn read(&self, off: u64, buf: &mut [u8]) -> Result<usize, Error> {
        let f = self.file.read().unwrap();
        f.read_at(off, buf)
    }

    async fn write(&self, off: u64, buf: &[u8]) -> Result<usize, Error> {
        let mut f = self.file.write().unwrap();
        f.write_at(off, buf)
    }
}

pub struct LocalDirIter {
    iter: std::iter::Skip<std::fs::ReadDir>,
    // the current offset of iter
    offset : i64
}

impl AsyncIterator for LocalDirIter {
    type Item = Result<(FileNameBuf, LocalFile), Error>;

    async fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|entry| {
            let entry = entry?;
            let path = entry.path();
            let name = FileNameBuf::from(path.file_name().unwrap());
            let file = LocalFile::new(path);
            self.offset += 1;
            Ok((name, file))
        })
    }
}

#[derive(Clone)]
pub struct LocalDirHandle {
    file: PathBuf
}

impl DirHandle<'_> for LocalDirHandle {
    type File = LocalFile;
    type Iterator = LocalDirIter;

    async fn at(&self, off : i64) -> Result<Self::Iterator, Error> {
        let iter = self.file.read_dir()?.skip(off as usize);
        Ok(LocalDirIter { iter, offset: off })
    }
}

impl std::fmt::Display for LocalDirHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "LocalDirHandle({})", self.file.display())
    }
}

#[derive(Clone)]
pub struct LocalFile {
    path: PathBuf,
    file_id: FileId
}

impl LocalFile {
    pub fn new(path: PathBuf) -> Self {
        let file_id = FileId::Uuid(Uuid::new_v5(&LOCAL_STORAGE_ID.0,
                    path.to_str().unwrap().as_bytes()));
        Self { path, file_id }
    }
}

pub const LOCAL_STORAGE_ID : StorageId = StorageId(Uuid::from_bytes([
    0x6c, 0xab, 0xbe, 0x19, 0xaf, 0x7d, 0x15, 0x21, 0x91, 0xbe, 0x01, 0xcd, 0x4f, 0xd4, 0x30,
    0xcc,
]));

impl<'fs> File<'fs> for LocalFile {
    type DirHandle = LocalDirHandle;
    type IOHandle = LocalIOHandle;

    fn file_id(&self) -> FileId { 
        self.file_id.clone()
    }

    fn storage_id(&self) -> StorageId { 
        LOCAL_STORAGE_ID.clone()
    }

    fn file_type(&self) -> FileType {
        if self.path.is_dir() { 
            FileType::Directory
        } else { 
            FileType::Regular
        }
    }

    async fn get_attr(&self, a: Attribute) -> Result<AttrValue, Error> {
        use Attribute::*;
        Ok(match a {
            Size => AttrValue::U64(fs::metadata(&self.path)?.len()),
            LastAccessed => AttrValue::Time(fs::metadata(&self.path)?.accessed()?),
            LastModified => AttrValue::Time(fs::metadata(&self.path)?.modified()?),
            LastChange => AttrValue::Time(fs::metadata(&self.path)?.modified()?),
            Created => AttrValue::Time(fs::metadata(&self.path)?.created()?),
            PosixPerm => AttrValue::U16(fs::metadata(&self.path)?.permissions().mode() as u16),
            PosixUid => AttrValue::U32(fs::metadata(&self.path)?.uid()),
            PosixGid => AttrValue::U32(fs::metadata(&self.path)?.uid()),
            _ => AttrValue::None // Does not have the attribute
        })
    }

    async fn set_attr(&self, _a: Attribute, _val: AttrValue) -> Result<(), Error> {
        todo!()
    }

    async fn children(&self) -> Result<Self::DirHandle, Error> {
        Ok(LocalDirHandle { file: self.path.clone() })
    }

    // remove a child
    async fn remove(&self, part: &FileName) -> Result<(), Error> {
        let full_path = self.path.join(part);
        fs::remove_file(full_path)
    }

    async fn put(&self, part: &FileName, handle : Self) -> Result<(), Error> {
        fs::rename(&handle.path, self.path.join(part))
    }
    async fn create(&self, part: FileNameBuf, file_type: FileType, _attrs: Vec<(Attribute, AttrValue)>) -> Result<Self, Error> {
        let path = self.path.join(part);
        if file_type == FileType::Directory {
            fs::create_dir(&path)?;
        } else {
            fs::File::create(&path)?;
        }
        Ok(LocalFile::new(path))
    }

    async fn get(&self, part: &FileName) -> Result<Option<Self>, Error> {
        let path = self.path.join(part);
        if path.exists() { Ok(Some(LocalFile::new(path))) } else { Ok(None) }
    }

    async fn mount<F: FileSystem>(&self, _fs: F) -> Result<(), Error> {
        Err(Error::from(ErrorKind::Unsupported))
    }
    async fn unmount(&self) -> Result<(), Error> {
        Err(Error::from(ErrorKind::Unsupported))
    }
    
    async fn open<'s>(&'s self, _flags: OpenFlags) -> Result<Self::IOHandle, Error> {
        let f = fs::File::open(self.path.clone())?;
        return Ok(LocalIOHandle { 
            file: Arc::new(RwLock::new(f))
        });
    }
}

impl FileSystem for LocalFS {
    type File<'fs> = LocalFile;

    fn root<'fs>(&'fs self) -> Result<Self::File<'fs>, Error> {
        Ok(LocalFile::new(self.root.clone()))
    }
}