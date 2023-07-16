use async_trait::async_trait;

use crate::{
    FileSystem, File, 
    FileId, StorageId, Error,
    Location, LocationBuf,
    IOHandle, FileIO,
    Attribute, AttrValue
};
use uuid::Uuid;
use std::path::PathBuf;

struct LocalFS {
    root: PathBuf,
}

#[derive(Clone)]
struct LocalFile {
    path: PathBuf,
}

#[async_trait]
impl<'fs> File<'fs> for LocalFile {
    fn id(&self) -> FileId { 
        FileId::Uuid(Uuid::new_v4()) 
    }
    fn storage_id(&self) -> StorageId { 
        StorageId(Uuid::new_v4()) 
    }

    fn is_dir(&self) -> bool { self.path.is_dir() }

    async fn get_attr(&self, a: Attribute) -> Result<AttrValue, Error> {
        todo!()
    }
    async fn set_attr(&self, a: Attribute, val: AttrValue) -> Result<(), Error> {
        todo!()
    }

    async fn children<I>(&self) -> Result<I, Error>
                where I: Iterator<Item=(LocationBuf, Self)> {
        todo!()
    }

    // remove a child
    async fn remove(&self, part: &Location) -> Result<(), Error> {
        Path::from(part).remove_file().await?;
        todo!()
    }

    async fn put(&self, part: &Location, handle : Self) -> Result<(), Error> {
        todo!()
    }
    async fn create(&self, part: &Location, is_dir: bool) -> Result<Self, Error> {
        todo!()
    }
    async fn get(&self, part: &Location) -> Result<Option<Self>, Error> {
        todo!()
    }
    async fn mount<F: FileSystem>(&self, fs: F) -> Result<(), Error> {
        todo!()
    }
    async fn unmount(&self) -> Result<(), Error> {
        todo!()
    }
    async fn open<'s>(&'s self) -> Result<FileIO<'s>, Error> {
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