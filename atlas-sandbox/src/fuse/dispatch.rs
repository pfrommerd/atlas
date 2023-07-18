use fuser::{spawn_mount2, BackgroundSession, MountOption, 
            ReplyEntry, ReplyOpen, ReplyDirectory,
            ReplyEmpty, ReplyAttr,
            Request as FuseRequest};
use std::path::{Path, PathBuf};

use async_trait::async_trait;
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

struct Request {
    info: RequestInfo,
    op: Op
}

#[derive(Debug)]
enum Op {
    Lookup(Lookup),
    Forget(Forget),
    GetAttr(GetAttr),
    OpenDir(OpenDir),
    ReleaseDir(ReleaseDir),
    ReadDir(ReadDir),
}

impl Request {
    fn new(r: &FuseRequest<'_>, op: Op) -> Self {
        let info = RequestInfo {
            unique: r.unique(),
            uid: r.uid(),
            gid: r.gid(),
            pid: r.pid(),
        };
        Self {
            info: info, op: op
        }
    }

    async fn handle<F: AsyncFilesystem>(self, fs: &F) {
        use Op::*;
        match self.op {
            Lookup(l) => fs.lookup(self.info, 
                l.parent, l.path, l.reply).await,
            Forget(f) => fs.forget(self.info,
                f.ino, f.nlookup).await,
            GetAttr(g) => fs.getattr(self.info,
                g.ino, g.reply).await,
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
#[async_trait(?Send)]
pub trait AsyncFilesystem {
    async fn lookup(&self, info: RequestInfo, parent: INode, path: PathBuf, reply: ReplyEntry);
    async fn forget(&self, info: RequestInfo, ino: INode, nlookup: u64);
    async fn getattr(&self, info: RequestInfo, ino: INode, reply: ReplyAttr);
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
}

pub struct AsyncSession<F: AsyncFilesystem> {
    fs: F,
    receiver: async_channel::Receiver<Request>,
    _session: BackgroundSession,
}

impl<F: AsyncFilesystem> AsyncSession<F> {
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