use fuser::{spawn_mount2, BackgroundSession, MountOption, 
            ReplyEntry, ReplyOpen, ReplyDirectory,
            ReplyEmpty, ReplyAttr,
            Request as FuseRequest};
use std::path::{Path, PathBuf};

use crate::Error;

pub type INode = u64;

#[derive(Debug)]
struct Lookup {
    parent: INode,
    path: PathBuf,
    reply: ReplyEntry
}

#[derive(Debug)]
struct Forget {
    ino: INode,
    nlookup: u64
}

#[derive(Debug)]
struct GetAttr {
    ino: INode,
    reply: ReplyAttr
}

#[derive(Debug)]
struct Open {
    ino: INode,
    flags: i32,
    reply: ReplyOpen 
}

#[derive(Debug)]
struct Read {
    ino: INode,
    fh: u64,
    offset: i64,
    size: u32,
    flags: i32,
    lock_owner: Option<u64>,
    reply: fuser::ReplyData
}

#[derive(Debug)]
struct Write {
    ino: INode,
    fh: u64,
    offset: i64,
    data: Vec<u8>,
    write_flags: u32,
    flags: i32,
    lock_owner: Option<u64>,
    reply: fuser::ReplyWrite
}

#[derive(Debug)]
struct Release {
    ino: INode,
    fh: u64,
    flags: i32,
    lock_owner: Option<u64>,
    flush: bool,
    reply: ReplyEmpty
}

#[derive(Debug)]
struct OpenDir {
    ino: INode,
    flags: i32,
    reply: ReplyOpen 
}

#[derive(Debug)]
struct ReleaseDir {
    ino: INode,
    fh: u64,
    flags: i32,
    reply: ReplyEmpty
}

#[derive(Debug)]
struct ReadDir {
    ino: INode,
    fh: u64,
    offset: i64,
    reply: ReplyDirectory
}

#[derive(Debug)]
enum Op {
    Lookup(Lookup),
    Forget(Forget),
    GetAttr(GetAttr),
    Open(Open),
    Read(Read),
    Write(Write),
    Release(Release),
    OpenDir(OpenDir),
    ReleaseDir(ReleaseDir),
    ReadDir(ReadDir),
}

#[derive(Debug)]
pub struct RequestInfo {
    unique: u64,
    uid: u32, gid: u32, pid: u32,
}

impl RequestInfo {
    pub fn unique(&self) -> u64 { self.unique }
    pub fn uid(&self) -> u32 { self.uid }
    pub fn gid(&self) -> u32 { self.gid }
    pub fn pid(&self) -> u32 { self.pid }
}

#[derive(Debug)]
struct Request {
    info: RequestInfo,
    op: Op
}


impl Request {
    fn new(r: &FuseRequest<'_>, op: Op) -> Self {
        let info = RequestInfo {
            unique: r.unique(),
            uid: r.uid(),
            gid: r.gid(),
            pid: r.pid(),
        };
        Self { info, op }
    }

    async fn handle<F: AsyncFuseFilesystem>(self, fs: &F) {
        use Op::*;
        match self.op {
            Lookup(l) => fs.lookup(self.info, 
                l.parent, l.path, l.reply).await,
            Forget(f) => fs.forget(self.info,
                f.ino, f.nlookup).await,
            GetAttr(g) => fs.getattr(self.info,
                g.ino, g.reply).await,
            Open(o) => fs.open(self.info, 
                o.ino, o.flags, o.reply).await,
            Read(r) => fs.read(self.info,
                r.ino, r.fh, r.offset, r.size, r.flags,
                r.lock_owner, r.reply).await,
            Write(w) => fs.write(self.info,
                w.ino, w.fh, w.offset, &w.data,
                w.write_flags, w.flags, w.lock_owner, w.reply).await,
            Release(r) => fs.release(self.info, r.ino, r.fh, r.flags,
                r.lock_owner, r.flush, r.reply).await,
            OpenDir(o) => fs.opendir(self.info, 
                o.ino, o.flags, o.reply).await,
            ReleaseDir(r) => fs.releasedir(self.info, 
                r.ino, r.fh, r.flags, r.reply).await,
            ReadDir(r) => fs.readdir(self.info,
                r.ino, r.fh, r.offset, r.reply).await,
        }
    }
}

// An asynchronous version of the Fuser Filesystem trait
pub trait AsyncFuseFilesystem {
    async fn lookup(&self, info: RequestInfo, parent: INode, path: PathBuf, reply: ReplyEntry);
    async fn forget(&self, info: RequestInfo, ino: INode, nlookup: u64);
    async fn getattr(&self, info: RequestInfo, ino: INode, reply: ReplyAttr);
    async fn open(&self, info: RequestInfo, ino: INode, flags: i32, reply: ReplyOpen);
    async fn read(&self, info: RequestInfo, ino: INode, fh: u64, offset: i64, size: u32, 
                    flags: i32, lock_owner: Option<u64>, reply: fuser::ReplyData);
    async fn write(&self, info: RequestInfo, ino: INode, fh: u64, offset: i64, data: &[u8], 
                    write_flags: u32, flags: i32, lock_owner: Option<u64>, reply: fuser::ReplyWrite);
    async fn release(&self, info: RequestInfo, ino: u64, fh: u64, flags: i32, 
                        lock_owner: Option<u64>, flush: bool, reply: ReplyEmpty);
    async fn opendir(&self, info: RequestInfo, ino: INode, flags: i32, reply: ReplyOpen);
    async fn releasedir(&self, info: RequestInfo, ino: INode, fs: u64, flags: i32, reply: ReplyEmpty);
    async fn readdir(&self, info: RequestInfo, ino: INode, fh: u64, offset: i64, reply: ReplyDirectory);
}

struct RequestDispatcher {
    channel: async_channel::Sender<Request>,
}

impl RequestDispatcher {
    fn send(&self, r: Request) {
        let _ = self.channel.try_send(r);
    }
}

#[allow(unused_variables)]
impl fuser::Filesystem for RequestDispatcher {
    fn lookup(&mut self, _req: &FuseRequest<'_>, 
            parent: u64, name: &std::ffi::OsStr, reply: ReplyEntry) {
        self.send(Request::new(_req, Op::Lookup(Lookup {
            parent, path: PathBuf::from(name), reply
        })))
    }

    fn forget(&mut self, _req: &FuseRequest<'_>, ino: u64, nlookup: u64) {
        self.send(Request::new(_req, Op::Forget(Forget {
            ino, nlookup
        })))
    }


    fn getattr(&mut self, req: &FuseRequest<'_>, ino: u64, reply: fuser::ReplyAttr) {
        self.send(Request::new(req, Op::GetAttr(GetAttr {
            ino, reply
        })))
    }

    fn opendir(&mut self, 
            req: &FuseRequest<'_>,
            ino: u64, flags: i32, reply: fuser::ReplyOpen) {
        self.send(Request::new(req, Op::OpenDir(OpenDir {
            ino, flags, reply
        })))
    }

    fn releasedir(
                &mut self,
                req: &FuseRequest<'_>,
                ino: u64,
                fh: u64,
                flags: i32,
                reply: fuser::ReplyEmpty) {
        self.send(Request::new(req, Op::ReleaseDir(ReleaseDir {
            ino, fh, flags, reply
        })))
    }
    fn readdir(
            &mut self,
            req: &FuseRequest<'_>,
            ino: u64,
            fh: u64,
            offset: i64,
            reply: fuser::ReplyDirectory) {
        self.send(Request::new(req, Op::ReadDir(ReadDir {
            ino, fh, offset, reply
        })))
    }
    fn open(&mut self, req: &FuseRequest<'_>, 
            ino: u64, flags: i32, reply: ReplyOpen) {
        self.send(Request::new(req, Op::Open(Open {
            ino, flags, reply
        })))
    }
    fn release(&mut self, req: &FuseRequest<'_>, 
                    ino: u64, fh: u64, flags: i32, 
                    lock_owner: Option<u64>, flush: bool, reply: ReplyEmpty) {
        self.send(Request::new(req, Op::Release(Release {
            ino, fh, flags, lock_owner, flush, reply
        })))
    }
    fn read(&mut self,
            req: &FuseRequest<'_>,
            ino: u64, fh: u64,
            offset: i64, size: u32,
            flags: i32, lock_owner: Option<u64>,
            reply: fuser::ReplyData) {
        self.send(Request::new(req, Op::Read(Read {
            ino, fh, offset, size, flags, lock_owner, reply
        })))
    }
    fn write(&mut self,
            req: &FuseRequest<'_>,
            ino: u64, fh: u64,
            offset: i64, data: &[u8],
            write_flags: u32, flags: i32,
            lock_owner: Option<u64>, reply: fuser::ReplyWrite) {
        self.send(Request::new(req, Op::Write(Write {
            ino, fh, offset, data: data.to_vec(),
            write_flags, flags, lock_owner, reply
        })))
    }
}

pub struct AsyncFuseSession<F: AsyncFuseFilesystem> {
    fs: F,
    receiver: async_channel::Receiver<Request>,
    _session: BackgroundSession,
}

impl<F: AsyncFuseFilesystem> AsyncFuseSession<F> {
    pub fn new<P: AsRef<Path>>(path: &P, fs: F, options: &[MountOption]) -> Result<Self, Error> {
        let (s, r) = async_channel::unbounded();
        let dispatcher = RequestDispatcher { channel: s };
        let session = spawn_mount2(dispatcher, path.as_ref(), options).unwrap();
        Ok(Self { fs, receiver: r, _session: session })
    }

    pub async fn run(&self) -> Result<(), Error> {
        // run the session
        while let Ok(r) = self.receiver.recv().await {
            r.handle(&self.fs).await;
        }
        Ok(())
    }
}