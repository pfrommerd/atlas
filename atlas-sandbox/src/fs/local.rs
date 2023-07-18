
use super::{
    FileSystem, File, 
    FileId, StorageId, Error,
    Location, LocationBuf,
    IOHandle, DirHandle, OpenFlags,
    Attribute, AttrValue
};
use positioned_io::ReadAt;
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

pub struct LocalIOHandle {
    file: fs::File,
}

impl IOHandle for LocalIOHandle {
    async fn read(&self, off: u64, buf: &mut [u8]) -> Result<usize, Error> {
        self.file.read_at(off, buf)
    }
}

pub struct LocalDirIter {
    iter: std::iter::Skip<std::fs::ReadDir>,
    // the current offset of iter
    offset : i64
}

impl async_iterator::Iterator for LocalDirIter {
    type Item = Result<(LocationBuf, LocalFile), Error>;

    async fn next(&mut self) -> Option<Self::Item> {
        self.iter.next().map(|entry| {
            let entry = entry?;
            let path = entry.path();
            let name = LocationBuf::from(path.file_name().unwrap());
            let file = LocalFile { path };
            self.offset += 1;
            Ok((name, file))
        })
    }
}

pub struct LocalDirHandle {
    file: PathBuf
}

impl DirHandle<'_> for LocalDirHandle {
    type FileType = LocalFile;
    type Iterator = LocalDirIter;

    async fn at(&self, off : i64) -> Result<Self::Iterator, Error> {
        let iter = self.file.read_dir()?.skip(off as usize);
        Ok(LocalDirIter { iter, offset: off })
    }
}

#[derive(Clone)]
pub struct LocalFile {
    path: PathBuf,
}

impl<'fs> File<'fs> for LocalFile {
    type DirHandle = LocalDirHandle;
    type IOHandle = LocalIOHandle;

    fn id(&self) -> FileId { 
        FileId::Uuid(Uuid::new_v4()) 
    }
    fn storage_id(&self) -> StorageId { 
        StorageId(Uuid::new_v4()) 
    }

    fn is_dir(&self) -> bool { self.path.is_dir() }

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
    async fn remove(&self, part: &Location) -> Result<(), Error> {
        let full_path = self.path.join(part);
        fs::remove_file(full_path)
    }

    async fn put(&self, part: &Location, handle : Self) -> Result<(), Error> {
        fs::rename(&handle.path, self.path.join(part))
    }
    async fn create(&self, part: &Location, is_dir: bool) -> Result<Self, Error> {
        let path = self.path.join(part);
        if is_dir {
            fs::create_dir(&path)?;
        } else {
            fs::File::create(&path)?;
        }
        Ok(LocalFile { path })
    }

    async fn get(&self, part: &Location) -> Result<Option<Self>, Error> {
        let path = self.path.join(part);
        if path.exists() { Ok(Some(LocalFile { path })) } else { Ok(None) }
    }

    async fn mount<F: FileSystem>(&self, _fs: F) -> Result<(), Error> {
        Err(Error::from(ErrorKind::Unsupported))
    }
    async fn unmount(&self) -> Result<(), Error> {
        Err(Error::from(ErrorKind::Unsupported))
    }
    
    async fn open<'s>(&'s self, _flags: OpenFlags) -> Result<Self::IOHandle, Error> {
        todo!()
    }
}

impl FileSystem for LocalFS {
    type FileType<'fs> = LocalFile;

    fn root<'fs>(&'fs self) -> Result<Self::FileType<'fs>, Error> {
        Ok(LocalFile {
            path: self.root.clone(),
        })
    }
}