use crate::{Error, FileSystem, util::AsyncIterator};
pub mod dispatch;
pub mod manager;


use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use dispatch::{AsyncFuseFilesystem, AsyncFuseSession, INode, RequestInfo};
use manager::{NodeManager, HandleManager};
use fuser::{ReplyEntry, ReplyOpen, ReplyEmpty, ReplyDirectory, FileAttr, MountOption};

use log::trace;

use crate::fs::{Attribute, File, FileType, DirHandle, IOHandle};


impl From<FileType> for fuser::FileType {
    fn from(ft: FileType) -> Self {
        match ft {
            FileType::Directory => fuser::FileType::Directory,
            FileType::Regular => fuser::FileType::RegularFile,
            FileType::Symlink => fuser::FileType::Symlink,
        }
    }
}

struct FuseFS<'fs, F: FileSystem> {
    #[allow(dead_code)]
    fs: &'fs F,
    inodes: NodeManager<F::File<'fs>>,
    dir_manager : HandleManager<<F::File<'fs> as File<'fs>>::DirHandle>,
    file_manager : HandleManager<<F::File<'fs> as File<'fs>>::IOHandle>
}

impl<'fs, F: FileSystem> FuseFS<'fs, F> {
    fn new(fs: &'fs F) -> Result<Self, Error> {
        let inodes = NodeManager::new();
        // create INode 0 for the root
        inodes.lookup(&fs.root()?);
        Ok(Self { fs, inodes,
            dir_manager: HandleManager::new(),
            file_manager: HandleManager::new()
        })
    }
}

async fn lookup_attr<'fs, F: File<'fs>>(inode: INode, f: F) -> Result<FileAttr, Error> {
    use Attribute::*;
    let size : u64 = f.get_attr_into(Size).await.unwrap_or(0);
    let atime = f.get_attr_into(LastAccessed).await.unwrap_or(SystemTime::UNIX_EPOCH);
    let mtime = f.get_attr_into(LastModified).await.unwrap_or(SystemTime::UNIX_EPOCH);
    let ctime = f.get_attr_into(LastChange).await.unwrap_or(SystemTime::UNIX_EPOCH);
    let crtime = f.get_attr_into(Created).await.unwrap_or(SystemTime::UNIX_EPOCH);
    // Posix attributes
    let perm = f.get_attr_into(PosixPerm).await.unwrap_or(0o755);
    let uid = f.get_attr_into(PosixUid).await.unwrap_or(0);
    let gid = f.get_attr_into(PosixGid).await.unwrap_or(0);
    Ok(FileAttr {
        ino: inode, size,
        blocks: size / 4096,
        atime, mtime, ctime, crtime,
        perm, uid, gid,
        kind: f.file_type().into(),
        nlink: 1,
        rdev: 0,
        blksize: 4096,
        flags: 0,
    })
}


#[allow(unused_variables)]
impl<'fs, F: FileSystem> AsyncFuseFilesystem for FuseFS<'fs, F> {
    async fn lookup(&self, info: RequestInfo, parent_ino: INode, 
                path: PathBuf, reply: ReplyEntry) {
        let unique = info.unique();
        let path = path.as_ref();
        let parent = self.inodes.get(parent_ino).unwrap();
        match parent.get(path).await {
        Ok(file) => {
            if let Some(file) = file {
                let inode = self.inodes.lookup(&file);
                let ttl = Duration::from_secs(1);
                let attr = lookup_attr(inode, file).await;
                if let Ok(attr) = attr {
                    trace!("LOOKUP({unique}): parent={parent_ino}, path={path:?}, child={inode}");
                    reply.entry(&ttl, &attr, inode);
                } else {
                    trace!("LOOKUP({unique}): parent={parent_ino}, path={path:?}, child={inode} -> error");
                    reply.error(libc::ENOENT)
                }
            } else {
                trace!("LOOKUP({unique}): parent={parent_ino}, path={path:?} -> error");
                reply.error(libc::ENOENT)
            }
        },
        Err(e) => {
            trace!("LOOKUP({unique}): parent={parent_ino}, path={path:?} -> error");
            reply.error(e.raw_os_error().unwrap_or(0))
        }
        }
    }

    async fn forget(&self, info: RequestInfo, ino: INode, nlookup: u64) {
        let unique = info.unique();
        let c = self.inodes.forget(ino, nlookup);
        if c == 0 {
            trace!("FORGET({unique}): ino={ino}, nlookup={nlookup} -> forgotten");
        } else {
            trace!("FORGET({unique}): ino={ino}, nlookup={nlookup} -> {c} references left");
        }
    }

    async fn getattr(&self, info: RequestInfo, ino: INode, reply: fuser::ReplyAttr) {
        let unique = info.unique();
        let file = self.inodes.get(ino);
        if let Some(file) = file {
            let attr = lookup_attr(ino, file).await;
            if let Ok(attr) = attr {
                trace!("GETATTR({unique}): ino={ino}, value={attr:?}");
                reply.attr(&Duration::from_secs(1), &attr);
            } else {
                trace!("GETATTR({unique}): ino={ino}, failed");
                reply.error(libc::ENOENT)
            }
        } else {
            reply.error(libc::ENOENT)
        }
    }

    async fn open(&self, info: RequestInfo, ino: INode, 
                flags: i32, reply: ReplyOpen) {
        let unique = info.unique();
        use crate::fs::OpenFlags;
        match self.inodes.get(ino) {
        Some(file) => {
            match file.open(OpenFlags::Read).await {
            Ok(io_handle) => {
                let fh = self.file_manager.insert(io_handle);
                trace!("OPEN({unique}): ino={ino} -> fh={fh}");
                reply.opened(fh, 0);
            },
            Err(e) => {
                trace!("OPEN({unique}): ino={ino} -> error={e}");
                reply.error(e.raw_os_error().unwrap_or(libc::ENOENT))
            }
            }
        },
        None => {
            trace!("OPEN({unique}): ino={ino} -> no such file");
            reply.error(libc::ENOENT)
        }
        }
    }

    async fn read(&self, info: RequestInfo, ino: INode, 
                  fh: u64, offset: i64, size: u32,
                  flags: i32, lock_owner: Option<u64>, reply: fuser::ReplyData) {
        let unique = info.unique();
        let io_handle = match self.file_manager.get(fh) {
            Some(io_handle) => io_handle,
            None => {
                trace!("READ({unique}): ino={ino}, fh={fh} -> no such file");
                reply.error(libc::ENOENT);
                return
            }
        };
        let io_handle = io_handle.value();
        let mut buf = vec![0; size as usize];
        match io_handle.read(offset as u64, &mut buf).await {
            Ok(n) => {
                trace!("READ({unique}): ino={ino}, fh={fh}, offset={offset}, size={size} -> {n} bytes");
                reply.data(&buf[..n]);
            },
            Err(e) => {
                trace!("READ({unique}): ino={ino}, fh={fh}, offset={offset}, size={size} -> error={e}");
                reply.error(e.raw_os_error().unwrap_or(libc::ENOENT));
            }
        }
    }

    async fn write(&self, info: RequestInfo, ino: INode, 
                   fh: u64, offset: i64, data: &[u8],
                   write_flags: u32, flags: i32,
                   lock_owner: Option<u64>, reply: fuser::ReplyWrite) {
        let unique = info.unique();
        let io_handle = match self.file_manager.get(fh) {
            Some(io_handle) => io_handle,
            None => {
                trace!("WRITE({unique}): ino={ino}, fh={fh} -> no such file");
                reply.error(libc::ENOENT);
                return
            }
        };
        let size = data.len();
        let io_handle = io_handle.value();
        match io_handle.write(offset as u64, data).await {
            Ok(n) => {
                trace!("WRITE({unique}): ino={ino}, fh={fh}, offset={offset}, size={size} -> {n} bytes");
                reply.written(n as u32);
            },
            Err(e) => {
                trace!("WRITE({unique}): ino={ino}, fh={fh}, offset={offset}, size={size} -> error={e}");
                reply.error(e.raw_os_error().unwrap_or(libc::ENOENT));
            }
        }
    }

    async fn release(&self, info: RequestInfo, ino: INode, 
                    fh: u64, _flags: i32, lock_owner: Option<u64>, flush: bool, reply: ReplyEmpty) {
        let unique = info.unique();
        trace!("RELEASE({unique}): ino={ino}, fh={fh}");
        self.file_manager.remove(fh);
        reply.ok();
    }

    // stateless directory I/O
    async fn opendir(&self, info: RequestInfo, ino: INode,
                _flags: i32, reply: ReplyOpen) {
        let unique = info.unique();
        let file = match self.inodes.get(ino) {
            Some(file) => file,
            None => {
                trace!("OPENDIR({unique}): ino={ino} -> no such file");
                reply.error(libc::ENOENT);
                return
            }
        };
        match file.children().await {
            Ok(dir_handle) => {
                let fh = self.dir_manager.insert(dir_handle);
                trace!("OPENDIR({unique}): ino={ino} -> fh={fh}");
                reply.opened(fh, 0);
            },
            Err(e) => {
                trace!("OPENDIR({unique}): ino={ino} -> err={e}");
                reply.error(e.raw_os_error().unwrap_or(libc::ENOENT))
            }
        }
    }

    async fn releasedir(&self, info: RequestInfo, ino: INode, 
                    fh: u64, _flags: i32, reply: ReplyEmpty) {
        let unique = info.unique();
        trace!("RELEASEDIR({unique}): ino={ino}, fh={fh}");
        self.dir_manager.remove(fh);
        reply.ok();
    }

    async fn readdir(&self, info: RequestInfo, ino: INode, 
                    fh: u64, offset: i64, mut reply: ReplyDirectory) {
        let unique = info.unique();
        let dir_handle = match self.dir_manager.get(fh) {
            Some(dir_handle) => dir_handle,
            None => {
                trace!("READDIR({unique}): ino={ino}, fh={fh} -> no such file");
                reply.error(libc::ENOENT);
                return
            }
        };
        let dir_handle = dir_handle.value();
        trace!("READDIR({unique}): ino={ino}, fh={fh}, offset={offset} -> handle={dir_handle}");
        let mut iter = match dir_handle.at(offset).await {
            Ok(iter) => iter,
            Err(e) => {
                trace!("READDIR({unique}): -> err={e}");
                reply.error(e.raw_os_error().unwrap_or(libc::ENOENT));
                return
            }
        };
        let mut idx = offset;
        let mut full = false;
        if idx == 0 {
            idx = idx + 1;
            full = reply.add(ino, idx, fuser::FileType::Directory, ".");
        }
        if idx == 1 && !full {
            idx = idx + 1;
            full = reply.add(ino, idx, fuser::FileType::Directory, "..");
        }
        while !full {
            let entry = iter.next().await;
            match entry {
                Some(Ok((name, file))) => {
                    let n = self.inodes.lookup(&file);
                    idx = idx + 1;
                    trace!("READDIR({unique}): -> idx={idx}, child={name:?}, ino={n}");
                    full = reply.add(n, idx, file.file_type().into(),
                        name
                    );
                },
                Some(Err(e)) => {
                    trace!("READDIR({unique}): -> err={e}");
                    reply.error(e.raw_os_error().unwrap_or(libc::ENOENT));
                    return
                }
                None => break
            }
        }
        trace!("READDIR({unique}): -> ok");
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