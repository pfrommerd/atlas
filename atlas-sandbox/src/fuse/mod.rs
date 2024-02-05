use crate::{Error, FileSystem, util::AsyncIterator};
pub mod dispatch;
pub mod manager;

use crate::fs::AttrValue;

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use dispatch::{AsyncFuseFilesystem, AsyncFuseSession, INode, RequestInfo};
use manager::{NodeManager, HandleManager};
use fuser::{ReplyEntry, ReplyOpen, ReplyEmpty, ReplyDirectory, FileAttr, MountOption};

use log::trace;

use crate::fs::{Attribute, File, DirHandle};

struct FuseFS<'fs, F: FileSystem> {
    fs: &'fs F,
    inodes: NodeManager<F::FileType<'fs>>,
    dir_manager : HandleManager<<F::FileType<'fs> as File<'fs>>::DirHandle>,
    #[allow(dead_code)]
    file_manager : HandleManager<<F::FileType<'fs> as File<'fs>>::IOHandle>
}

impl<'fs, F: FileSystem> FuseFS<'fs, F> {
    fn new(fs: &'fs F) -> Result<Self, Error> {
        let inodes = NodeManager::new();
        // create INode 0 for the root
        inodes.request(&fs.root()?);
        Ok(Self { fs, inodes,
            dir_manager: HandleManager::new(),
            file_manager: HandleManager::new()
        })
    }
}

async fn lookup_attr<'fs, F: File<'fs>>(inode: INode, f: F) -> Result<FileAttr, Error> {
    use Attribute::*;
    let size = f.get_attr(Size).await;
    let atime = f.get_attr(LastAccessed).await
            .unwrap_or(AttrValue::Time(SystemTime::UNIX_EPOCH))
            .into_time().unwrap_or(SystemTime::UNIX_EPOCH);
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


#[allow(unused_variables)]
impl<'fs, F: FileSystem> AsyncFuseFilesystem for FuseFS<'fs, F> {
    async fn lookup(&self, _info: RequestInfo, parent: INode, 
                path: PathBuf, reply: ReplyEntry) {
        let path = path.as_ref();
        trace!("lookup: parent={parent}, path={path:?}");
        let parent = self.inodes.get(parent).unwrap();
        match parent.get(path).await {
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
        trace!("getattr: ino={ino}");
        let file = self.inodes.get(ino);
        if let Ok(file) = file {
            let attr = lookup_attr(ino, file).await;
            if let Ok(attr) = attr {
                reply.attr(&Duration::from_secs(1), &attr);
            } else {
                trace!("getattr: ino={ino}, failed");
                reply.error(libc::ENOENT)
            }
        } else {
            reply.error(libc::ENOENT)
        }
    }

    async fn open(&self, _info: RequestInfo, ino: INode, 
                flags: i32, reply: ReplyOpen) {
        use crate::fs::OpenFlags;
        let file = self.inodes.get(ino).unwrap();
        let fh = self.file_manager.insert(file.open(OpenFlags::Read).await.unwrap());
        reply.opened(fh, 0);
    }

    async fn release(&self, _info: RequestInfo, _ino: INode, 
                    fh: u64, _flags: i32, lock_owner: Option<u64>, flush: bool, reply: ReplyEmpty) {
        reply.ok();
    }

    // stateless directory I/O
    async fn opendir(&self, _info: RequestInfo, ino: INode,
                _flags: i32, reply: ReplyOpen) {
        let dir_handle = self.inodes.get(ino).unwrap().children().await.unwrap();
        let fh = self.dir_manager.insert(dir_handle);
        reply.opened(fh, 0);
    }

    async fn releasedir(&self, _info: RequestInfo, _ino: INode, 
                    fh: u64, _flags: i32, reply: ReplyEmpty) {
        self.dir_manager.remove(fh);
        reply.ok();
    }

    async fn readdir(&self, _info: RequestInfo, _ino: INode, 
                    fh: u64, offset: i64, mut reply: ReplyDirectory) {
        let dir_handle = self.dir_manager.get(fh).unwrap();
        let mut iter = dir_handle.at(offset).await.unwrap();
        let mut idx = offset;
        loop {
            let entry = iter.next().await;
            match entry {
                Some(entry) => {
                    let (name, file) = entry.unwrap();
                    let n = self.inodes.request(&file);
                    idx = idx + 1;
                    let full = reply.add(n, idx,
                        if file.is_dir() { fuser::FileType::Directory }
                        else { fuser::FileType::RegularFile },
                        name
                    );
                    if full { break }
                },
                None => break
            }
        }
        reply.ok();
    }
}

pub struct FuseServer<'fs, F: FileSystem> {
    session: AsyncFuseSession<FuseFS<'fs, F>>
}

impl<'fs, F: FileSystem> FuseServer<'fs, F> {
    pub fn new<P: AsRef<Path>>(path: &P, fs: &'fs F, options: &[MountOption]) -> Result<Self, Error> {
        let fuse_fs = FuseFS::new(fs)?;
        let mut options : Vec<MountOption> = options.iter().cloned().collect();
        options.push(MountOption::DefaultPermissions);
        let session = AsyncFuseSession::new(path, fuse_fs, &options)?;
        Ok(Self { session })
    }

    pub async fn run(&self) -> Result<(), Error> {
        self.session.run().await
    }
}