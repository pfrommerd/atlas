use crate::{Error, FileSystem};
pub mod dispatch;
pub mod manager;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use dispatch::{AsyncFilesystem, AsyncSession, INode, RequestInfo};
use manager::NodeManager;
use fuser::{ReplyEntry, ReplyOpen, ReplyEmpty, ReplyDirectory, FileAttr, MountOption};

use async_trait::async_trait;

use crate::File;
use crate::Attribute;

struct FuseFS<'fs, F: FileSystem> {
    #[allow(dead_code)]
    fs: &'fs F,
    inodes: NodeManager<F::FileType<'fs>>
}

impl<'fs, F: FileSystem> FuseFS<'fs, F> {
    fn new(fs: &'fs F) -> Result<Self, Error> {
        let inodes = NodeManager::new();
        // create INode 0 for the root
        inodes.request(&fs.root()?);
        Ok(Self { fs, inodes })
    }
}

async fn lookup_attr<'fs, F: File<'fs>>(inode: INode, f: F) -> Result<FileAttr, Error> {
    use Attribute::*;
    let size = f.get_attr(Size).await?.into_u64().unwrap_or(0);
    let atime = f.get_attr(LastAccessed).await?.into_time().unwrap_or(SystemTime::UNIX_EPOCH);
    let mtime = f.get_attr(LastModified).await?.into_time().unwrap_or(SystemTime::UNIX_EPOCH);
    let ctime = f.get_attr(LastChange).await?.into_time().unwrap_or(SystemTime::UNIX_EPOCH);
    let crtime = f.get_attr(Created).await?.into_time().unwrap_or(SystemTime::UNIX_EPOCH);
    // Posix attributes
    let perm = f.get_attr(PosixPerm).await?.into_u16().unwrap_or(0o755);
    let uid = f.get_attr(PosixUid).await?.into_u32().unwrap_or(0);
    let gid = f.get_attr(PosixGid).await?.into_u32().unwrap_or(0);
    Ok(FileAttr {
        ino: inode,
        size: size,
        blocks: size / 4096,
        atime: atime,
        mtime: mtime,
        ctime: ctime,
        crtime: crtime,
        kind: if f.is_dir() { fuser::FileType::Directory }
              else { fuser::FileType::RegularFile },
        perm: perm,
        nlink: 1,
        uid: uid,
        gid: gid,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    })
}

#[async_trait(?Send)]
impl<'fs, F: FileSystem> AsyncFilesystem for FuseFS<'fs, F> {
    async fn lookup(&self, _info: RequestInfo, parent: INode, 
                path: PathBuf, reply: ReplyEntry) {
        let parent = self.inodes.get(parent).unwrap();
        match parent.get(path.as_ref()).await {
        Ok(file) => {
            if let Some(file) = file {
                let inode = self.inodes.request(&file);
                let ttl = Duration::from_secs(1);
                let attr = lookup_attr(inode, file).await;
                if let Ok(attr) = attr {
                    reply.entry(&ttl, &attr, inode);
                } else {
                    reply.error(libc::ENOENT)
                }
            } else {
                reply.error(libc::ENOENT)
            }
        },
        Err(e) => reply.error(e.raw_os_error().unwrap_or(0))
        }
    }

    async fn forget(&self, _info: RequestInfo, ino: INode, nlookup: u64) {
        self.inodes.forget(ino, nlookup);
    }

    async fn getattr(&self, _info: RequestInfo, ino: INode, reply: fuser::ReplyAttr) {
        let file = self.inodes.get(ino).unwrap();
        let attr = lookup_attr(ino, file).await;
        if let Ok(attr) = attr {
            reply.attr(&Duration::from_secs(1), &attr);
        } else {
            reply.error(libc::ENOENT)
        }
    }

    // stateless directory I/O
    async fn opendir(&self, _info: RequestInfo, _ino: INode,
                _flags: i32, reply: ReplyOpen) {
        reply.opened(0, 0)
    }

    #[allow(unused_variables)]
    async fn releasedir(&self, _info: RequestInfo, _ino: INode, 
                    fh: u64, _flags: i32, reply: ReplyEmpty) {
        reply.ok();
    }
    #[allow(unused_variables)]
    async fn readdir(&self, _info: RequestInfo, _ino: INode, 
                    fh: u64, _offset: i64, _reply: ReplyDirectory) {
    }
}

pub struct FuseServer<'fs, F: FileSystem> {
    session: AsyncSession<FuseFS<'fs, F>>
}

impl<'fs, F: FileSystem> FuseServer<'fs, F> {
    pub fn new<P: AsRef<Path>>(path: &P, fs: &'fs F, options: &[MountOption]) -> Result<Self, Error> {
        let fuse_fs = FuseFS::new(fs)?;
        let mut options : Vec<MountOption> = options.iter().cloned().collect();
        options.push(MountOption::DefaultPermissions);
        let session = AsyncSession::new(path, fuse_fs, &options)?;
        Ok(Self { session })
    }

    pub async fn run(&self) -> Result<(), Error> {
        self.session.run().await
    }
}