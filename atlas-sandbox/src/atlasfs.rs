use fuser::{Request, ReplyEntry, ReplyDirectory, FileAttr, FileType,
            ReplyData, ReplyAttr, MountOption,
            BackgroundSession};
use std::ffi::{OsStr, OsString};
use std::collections::HashMap;

use std::cell::{Cell, RefCell};

use std::ops::Deref;

use atlas_core::store::{Storage, Handle, ReaderWhich, 
    ObjectReader, RecordReader, StringReader, BufferReader};
use atlas_core::vm::Machine;
use atlas_core::{Error, Result, ErrorKind};

use std::path::Path;
use std::time::{Duration, SystemTime};
use std::borrow::Borrow;

type Inode = u64;

use libc::ENOENT;

struct FsNode<'s, S: Storage + 's> {
    handle: S::Handle<'s>,
    uses: Cell<usize>, // usage counter. When zero, clean up the node mapping
}

pub struct AtlasFS<'m, 's, S: Storage> {
    machine: &'m Machine<'s, S>,
    ctime: SystemTime,
    uid: u32,
    gid: u32,
    // Map back from Inode to handle and back
    inode_map: RefCell<HashMap<Inode, FsNode<'s, S>>>,
    handle_map: RefCell<HashMap<S::Handle<'s>, Inode>>,
    inode_counter: Cell<Inode>,
    // The channel on which we get our requests
    requests: async_channel::Receiver<FsRequest>,
    // The background thread feeding us requests
    _session: BackgroundSession
}

const TTL : Duration = Duration::from_secs(0);

impl<'m, 's, S: Storage> AtlasFS<'m, 's, S> {
    pub fn new(mount_point: impl AsRef<Path>, stype_name: String,
            machine: &'m Machine<'s, S>, root: S::Handle<'s>) -> Result<AtlasFS<'m, 's, S>> {
        // Make the channels
        let (sender, recv) = async_channel::unbounded();
        let handler = FuseHandler { sender };
        let options = &[MountOption::FSName("atlasfs".to_string()),
                        MountOption::Subtype(stype_name),
                        MountOption::RO,
                        MountOption::DefaultPermissions];
        let session = fuser::spawn_mount2(handler, mount_point, options)?;
        let mut inode_map = HashMap::new();
        let mut handle_map = HashMap::new();
        inode_map.insert(1 as Inode, FsNode { handle: root.clone(), uses: Cell::new(1) });
        handle_map.insert(root, 1 as Inode);
        Ok(Self {
            machine,
            uid: users::get_current_uid(),
            gid: users::get_current_gid(),
            ctime: SystemTime::now(),
            inode_map: RefCell::new(inode_map),
            handle_map: RefCell::new(handle_map),
            inode_counter: Cell::new(1),
            requests: recv,
            _session: session
        })
    }

    // Will handle requests in a loop
    pub async fn handle_requests(&self) {
        loop {
            let request = self.requests.recv().await;
            if let Ok(request) = request {
                use FsRequest::*;
                match request {
                    Read(r) => self.handle_read(r).await,
                    Lookup(l) => self.handle_lookup(l).await,
                    ReadDir(r) => self.handle_read_dir(r).await,
                    Attr(a) => self.handle_getattr(a).await
                }
            } else {
                return;
            }
        }
    }

    fn create_ino(&self, handle: &S::Handle<'s>) -> Result<Inode> {
        let mut handle_map = self.handle_map.borrow_mut();
        let mut inode_map = self.inode_map.borrow_mut();
        match handle_map.get(&handle) {
            Some(e) => {
                let fs = inode_map.get(e).unwrap();
                fs.uses.set(fs.uses.get() + 1);
                Ok(*e)
            },
            None => {
                let ino = self.inode_counter.get() + 1;
                self.inode_counter.set(ino);
                inode_map.insert(ino, FsNode { handle: handle.clone(), uses: Cell::new(1) });
                handle_map.insert(handle.clone(), ino);
                Ok(ino)
            }
        }
    }

    async fn get_attrs(&self, handle: &S::Handle<'s>, ino: Inode) -> Result<FileAttr> {
        let file = handle.reader()?.as_record()?;
        let directory_flag = file.get("entries").is_ok();
        let executable_flag : Result<bool> = try {
            if !directory_flag {
                let executable_flag = file.get("executable")?;
                let executable_flag = self.machine.force(executable_flag.borrow()).await?;
                let r = executable_flag.reader()?;
                r.as_bool()?
            } else { false }
        };
        let executable_flag = executable_flag.unwrap_or(false);

        let size = if !directory_flag {
            let size = file.get("size");
            match size {
                Ok(size) => {
                    let size = self.machine.force(size.borrow()).await?;
                    let size = size.reader()?.as_int()?;
                    size as usize
                },
                Err(_) => {
                    // Calculate the size from the content
                    let content = file.get("content")?;
                    let content = self.machine.force(content.borrow()).await?;
                    let content = content.reader()?;
                    use ReaderWhich::*;
                    let size = match content.which() {
                    Buffer(buff) => buff.len(),
                    String(str) => str.len(),
                    _ => {
                        log::error!("Error during read, wrong content type");
                        return Err(Error::new("Content must be buffer or string"));
                    }
                    };
                    size
                }
            }
        } else { 4096 };

        let perm = if directory_flag { 0o755 } else {
            0o644 | if executable_flag { 0o111 } else { 0 }
        };

        Ok(FileAttr {
            ino,
            size : size as u64,
            blocks: 1,
            atime: self.ctime,
            mtime: self.ctime,
            ctime: self.ctime,
            crtime: self.ctime,
            kind: if directory_flag { FileType::Directory } else { FileType::RegularFile },
            perm,
            nlink: 1,
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            flags: 0,
            blksize: 512
        })
    }

    async fn handle_read(&self, req: ReadRequest) {
        let content : Result<S::Handle<'s>> = try {
            let inode_map = self.inode_map.borrow();
            let file = inode_map.get(&req.inode)
                .ok_or(Error::new("No such node"))?;
            let handle = &file.handle;
            let handle = self.machine.force(handle).await?;
            let file = self.machine.force(&handle).await?;
            let file = file.reader()?.as_record()?;
            let content = file.get("content")?;
            self.machine.force(content.borrow()).await?
        };
        let content = match content {
            Ok(c) => c,
            Err(e) => {
                log::error!("Error during read {:?}", e);
                req.reply.error(ENOENT);
                return;
            }
        };
        let content = match content.reader() {
            Ok(c) => c,
            Err(e) => {
                log::error!("Error during read {:?}", e);
                req.reply.error(ENOENT);
                return;
            }
        };
        use ReaderWhich::*;
        match content.which() {
        Buffer(buff) => {
            let (start, len) = (req.off as usize, std::cmp::min(req.size as usize, buff.len() - req.off as usize));
            let buff = buff.slice(start, len);
            req.reply.data(buff.deref());
        },
        String(str) => {
            let (start, len) = (req.off as usize, std::cmp::min(req.size as usize, str.len() - req.off as usize));
            let buff = str.slice(start, len);
            req.reply.data(buff.deref().as_bytes());
        },
        _ => {
            log::error!("Error during read, wrong content type");
            req.reply.error(ENOENT);
            return;
        }
        };
    }

    async fn handle_read_dir(&self, mut req: ReadDirRequest) {
        let res: Result<Vec<(Inode, FileType, String)>> = try {
            let dir_fs = {
                let inode_map = self.inode_map.borrow();
                inode_map.get(&req.inode).ok_or(Error::new("No such node"))?.handle.clone()
            };
            let dir = self.machine.force(&dir_fs).await?;
            let dir = dir.reader()?.as_record()?;
            let entries = dir.get("entries")?;
            let entries = self.machine.force(entries.borrow()).await?;
            let entries = entries.reader()?.as_record()?;

            let mut res = Vec::new();
            res.push((req.inode, FileType::Directory, ".".to_string()));
            res.push((req.inode, FileType::Directory, "..".to_string()));
            for (k, v) in entries.iter() {
                let str = k.borrow().reader()?.as_string()?;
                let str = str.as_slice();
                let file = self.machine.force(v.borrow()).await?;
                let ino = self.create_ino(&file)?;
                let is_dir = file.reader()?.as_record()?.get("entries").is_ok();
                res.push((ino, if is_dir { FileType::Directory } else { FileType::RegularFile }, str.to_string()))
            }
            res
        };
        match res {
            Ok(entries) => {
                for (i, entry) in entries.into_iter().enumerate().skip(req.off as usize) {
                    if req.reply.add(entry.0, (i + 1) as i64, entry.1, entry.2) {
                        break;
                    }
                }
                req.reply.ok();
            },
            Err(e) => {
                log::error!("Error during readdir {:?}", e);
                req.reply.error(ENOENT);
            }
        }
    }

    async fn handle_getattr(&self, req: AttrRequest) {
        let res: Result<FileAttr> = try {
            let inode_map = self.inode_map.borrow();
            let file = inode_map.get(&req.inode)
                .ok_or(Error::new("No such node"))?;
            let handle = file.handle.borrow();
            let handle = self.machine.force(handle).await?;
            self.get_attrs(&handle, req.inode).await?
        };
        match res {
            Ok(a) => req.reply.attr(&TTL, &a),
            Err(e) => {
                log::error!(target: "atlasfs", "Error during getatr {:?}", e);
                req.reply.error(ENOENT);
            }
        }
    }

    async fn handle_lookup(&self, req: LookupRequest) {
        let name = req.name.as_os_str().to_str().ok_or(Error::new("Bad file name"));
        let name = match name {
            Ok(o) => o,
            Err(_) => {
                req.reply.error(ENOENT);
                return;
            }
        };
        let res: Result<FileAttr> = try {
            log::trace!(target: "atlasfs", "looking up child {} of {}", name, req.parent);
            let parent_handle = {
                let inode_map = self.inode_map.borrow();
                inode_map.get(&req.parent).ok_or(Error::new("No such node"))?.handle.clone()
            };
            // make sure we have a directory
            let parent_handle = self.machine.force(&parent_handle).await?;
            let parent_record =  parent_handle.reader()?.as_record()?;

            let entries = parent_record.get("entries")?;
            let entries = self.machine.force(entries.borrow()).await?;
            let entries = entries.reader()?.as_record()?;

            let child = entries.get(name)
                .map_err(|_| Error::new_const(ErrorKind::Filesystem, "Child not found"))?;
            let child_record = self.machine.force(child.borrow()).await?;
            let ino = self.create_ino(&child_record)?;
            self.get_attrs(&child_record, ino).await?
        };
        match res {
            Ok(a) => req.reply.entry(&TTL, &a, 0),
            Err(e) => {
                if e.kind() != ErrorKind::Filesystem {
                    log::error!(target: "atlasfs", "Error during lookup {:?}", e);
                } else {
                    log::trace!(target: "atlasfs", "File not found {}", name);
                }
                req.reply.error(ENOENT);
            }
        }
    }
}

struct LookupRequest {
    parent: Inode,
    name: OsString,
    reply: ReplyEntry
}

struct ReadRequest {
    inode: Inode,
    off: i64,
    size: usize,
    reply: ReplyData
}

struct ReadDirRequest {
    inode: Inode,
    off: i64,
    reply: ReplyDirectory
}

struct AttrRequest {
    inode: Inode,
    reply: ReplyAttr
}

enum FsRequest {
    Lookup(LookupRequest),
    Read(ReadRequest),
    ReadDir(ReadDirRequest),
    Attr(AttrRequest)
}

pub struct FuseHandler {
    sender: async_channel::Sender<FsRequest>,
}

impl fuser::Filesystem for FuseHandler {
    fn lookup(&mut self, _req: &Request, parent: Inode, name: &OsStr, reply: ReplyEntry) {
        let req = LookupRequest{ parent, name: name.to_os_string(), reply };
        if let Err(_) = self.sender.try_send(FsRequest::Lookup(req)) {
            log::error!("Failed to handle fuse request!");
        }
    }
    fn read(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, size: u32, 
                _flags: i32, _lock: Option<u64>, reply: ReplyData) {
        let req = ReadRequest { inode: ino, off: offset, size: size as usize, reply };
        if let Err(_) = self.sender.try_send(FsRequest::Read(req)) {
            log::error!("Failed to handle fuse request!");
        }
    }
    fn readdir(&mut self, _req : &Request, ino: u64, _fh: u64, offset: i64, reply: ReplyDirectory) {
        let req = ReadDirRequest { inode: ino, off: offset, reply };
        if let Err(_) = self.sender.try_send(FsRequest::ReadDir(req)) {
            log::error!("Failed to handle fuse request!");
        }
    }
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        let req = AttrRequest { inode: ino, reply };
        if let Err(_) = self.sender.try_send(FsRequest::Attr(req)) {
            log::error!("Failed to handle fuse request!");
        }
    }
}