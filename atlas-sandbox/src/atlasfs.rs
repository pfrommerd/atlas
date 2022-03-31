use bimap::hash::BiHashMap;
use fuser::{Request, ReplyEntry, ReplyDirectory, 
            ReplyData, FileAttr, FileType, ReplyAttr, MountOption,
            BackgroundSession};
use std::{time::SystemTime};

type Inode = u64;

pub struct AtlasFS<'s, 'e, 'er, C: Cache<'s, S>, S: Storage> {
    machine: Machine<'s, C, S>,
    timestamp: SystemTime,
    inode_counter: u64
}

pub struct Sandbox<'s, C: Cache<'s, S>, S: Storage>{
    filesystem: AtlasFS<'s, C, S>,
    path: String
}

use std::sync::mpsc;

pub struct AtlasFS<'s, 'm, C: Cache<'s, S>, S: Storage> {
    root: S::Handle<'s>,
    machine: &'m Machine<'s, C, S>,
    inode_map: BiHashMap<INodeID, S::Handle<'s>>,
    // Request receivers
    requests: async_channel::Receiver<FsRequest>,
    session: BackgroundSession
}

impl<'s, 'm, C: Cache<'s, S>, S: Storage> AtlasFs<'s, 'm, C, S> {
    fn new(mount_point: impl AsRef<Path>, stype_name: String,
            machine: &'m Machine<'s, C, S>, root: S::Handle<'s>) -> Result<Self, Error> {
        // Make the channels
        let (sender, recv) = async_channel::unbounded();
        let handler = FuseHandler { sender };
        let options = &[MountOption::FSName("atlasfs".to_string()),
                        MountOption::Subtype(stype_name),
                        MountOption::AllowRoot,
                        MountOption::AutoUnmount,
                        MountOption::DefaultPermissions];
        let session = fuser::spawn_mount2(handler, mount_point, options)?;
        Self {
            root, machine,
            inode_map: BiHashMap::new(),
            requests: recv,
            session
        }
    }

    // Will handle requests in a loop
    async fn handle_requests() {
        loop {
            let request = self.requests.recv().await;
        }
    }

    async fn handle_read(req: ReadRequest) {

    }

    async fn handle_read_dir(req: ReadDirRequest) {

    }

    async fn handle_attr(req: AttrRequest) {

    }

    async fn handle_lookup(req: LookupRequest) {

    }
}

struct LookupRequest {
    inode: Inode,
    child: OsString,
    reply: ReplyEntry
}

struct ReadRequest {
    inode: Inode,
    off: u64,
    size: usize,
    reply: ReplyData
}

struct ReadDirRequest {
    inode: Inode,
    reply: ReplyDirectory
}

struct AttrRequest {
    inode: Inode,
    reply: ReplyAttr
}

enum FsRequest {
    Lookup(Lookuprequest),
    Read(ReadRequest),
    ReadDir(ReadDirRequest),
    Attr(AttrRequest)
}

pub struct FuseHandler {
    sender: async_channel::Sender<FsRequest>,
}

impl fuser::Filesystem for FuseHandler {
    fn lookup(&mut self, _req: &Request, parent: INodeID, name: &OsStr, reply: ReplyEntry) {
    }
}